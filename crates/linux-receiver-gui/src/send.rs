//! The Send side of the window. The sender itself is the `meshwav-agent` systemd user service; this
//! panel starts and stops that service and talks to it over its owner-only control socket (the same
//! one `meshwav devices | use | rename | pair` use). Audio keeps flowing when the window is closed.
use super::{ACCENT, BAD, MUTED, WARN};
use eframe::egui::{self, RichText};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

pub const UNIT: &str = "meshwav-agent";
const PAIR_WINDOW: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ServiceState {
    NotInstalled,
    Stopped,
    Running,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Device {
    pub name: String,
    pub key: String,
    pub state: String,
    pub chosen: bool,
}

/// The lines of the agent's `devices` reply: `<name padded to 20> <16 hex>  <state>[  (chosen)]`.
pub fn parse_devices(text: &str) -> Vec<Device> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim_end();
            let (l, chosen) = match l.strip_suffix("(chosen)") {
                Some(r) => (r.trim_end(), true),
                None => (l, false),
            };
            let (rest, state) = l.rsplit_once(' ')?;
            let (name, key) = rest.trim_end().rsplit_once(' ')?;
            if key.len() != 16 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            Some(Device { name: name.trim().to_string(), key: key.to_string(), state: state.to_string(), chosen })
        })
        .collect()
}

/// The 6-digit code and the link from the agent's reply to `pair`.
pub fn parse_pairing(text: &str) -> Option<(String, String)> {
    let code = text.strip_prefix("pairing code ")?.get(..6)?.to_string();
    if !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let link = text.lines().find_map(|l| l.strip_prefix("pair: "))?.trim().to_string();
    Some((code, link))
}

/// The sender's key fingerprint from its pairing link, or an empty string if the link has no valid key.
pub fn fingerprint_of_link(link: &str) -> String {
    meshwav_receiver::store::parse_pair_link(link).map(|l| meshwav_receiver::discover::Found { addr: std::net::SocketAddr::from(([0, 0, 0, 0], 0)), key: l.key }.fingerprint()).unwrap_or_default()
}

/// One request to the running agent.
pub fn control(sock: &Path, line: &str) -> Result<String, String> {
    let mut s = UnixStream::connect(sock).map_err(|e| format!("cannot reach the sender ({e})"))?;
    let t = Some(Duration::from_secs(2));
    let _ = s.set_read_timeout(t);
    let _ = s.set_write_timeout(t);
    writeln!(s, "{line}").map_err(|e| e.to_string())?;
    let mut out = String::new();
    s.read_to_string(&mut out).map_err(|e| e.to_string())?;
    Ok(out.trim_end().to_string())
}

fn systemctl(args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("systemctl").arg("--user").args(args).output().map_err(|e| format!("systemctl: {e}"))
}

pub fn service_state() -> ServiceState {
    let active = systemctl(&["is-active", UNIT]).map(|o| matches!(String::from_utf8_lossy(&o.stdout).trim(), "active" | "activating")).unwrap_or(false);
    if active {
        ServiceState::Running
    } else if systemctl(&["cat", UNIT]).map(|o| o.status.success()).unwrap_or(false) {
        ServiceState::Stopped
    } else {
        ServiceState::NotInstalled
    }
}

fn run_systemctl(args: &[&str]) -> Result<(), String> {
    let o = systemctl(args)?;
    if o.status.success() {
        Ok(())
    } else {
        Err(format!("systemctl --user {} failed: {}", args.join(" "), String::from_utf8_lossy(&o.stderr).trim()))
    }
}

pub fn start_service() -> Result<(), String> {
    run_systemctl(&["start", UNIT])
}

pub fn stop_service() -> Result<(), String> {
    run_systemctl(&["stop", UNIT])
}

/// The user unit for a copy of meshwav that is not installed from a package: the packaged unit with
/// `ExecStart` pointing at this program's `send` command.
pub fn unit_text(exe: &Path) -> String {
    include_str!("../../../packaging/systemd/meshwav-agent.service").replace("ExecStart=/usr/bin/meshwav send", &format!("ExecStart={} send", exe.display()))
}

pub fn install_service(exe: &Path) -> Result<(), String> {
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("no HOME")?);
    let dir = home.join(".config/systemd/user");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(format!("{UNIT}.service")), unit_text(exe)).map_err(|e| e.to_string())?;
    run_systemctl(&["daemon-reload"])?;
    run_systemctl(&["enable", "--now", UNIT])
}

/// The value of `key` in the agent's flat `key = value` settings file, if set.
pub fn get_key(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let l = l.split('#').next().unwrap_or("").trim();
        let (k, v) = l.split_once('=')?;
        (k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
    })
}

/// Set (or with `None`, remove) `key`, leaving every other line and comment as it was.
pub fn set_key(text: &str, key: &str, val: Option<&str>) -> String {
    let mut out = Vec::new();
    let mut done = false;
    for l in text.lines() {
        let is_key = l.trim_start().strip_prefix(key).is_some_and(|r| r.trim_start().starts_with('='));
        if !is_key {
            out.push(l.to_string());
        } else if let (Some(v), false) = (val, done) {
            out.push(format!("{key} = {v}"));
            done = true;
        }
    }
    if let (Some(v), false) = (val, done) {
        out.push(format!("{key} = {v}"));
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

struct OpenCode {
    code: String,
    link: String,
    /// The sender's key fingerprint (from the link), shown so receivers can be confirmed against it.
    fingerprint: String,
    until: Instant,
    devices_before: usize,
}

pub struct SendPanel {
    pub service: ServiceState,
    devices: Vec<Device>,
    code: Option<OpenCode>,
    rename: Option<(String, String)>,
    confirm_remove: Option<String>,
    last: Instant,
    message: Option<(String, bool)>,
    sock: PathBuf,
    config: PathBuf,
    exe: PathBuf,
    kbps: u32,
    frame_ms: u32,
    lossless: bool,
    keep_local: bool,
}

impl SendPanel {
    /// `dir` is `~/.config/meshwav`.
    pub fn new(dir: &Path) -> SendPanel {
        let config = dir.join("config.toml");
        let text = std::fs::read_to_string(&config).unwrap_or_default();
        SendPanel {
            service: service_state(),
            devices: Vec::new(),
            code: None,
            rename: None,
            confirm_remove: None,
            last: Instant::now() - Duration::from_secs(10),
            message: None,
            sock: dir.join("control.sock"),
            config,
            exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("meshwav")),
            kbps: get_key(&text, "kbps").and_then(|v| v.parse().ok()).unwrap_or(160).clamp(64, 256),
            frame_ms: get_key(&text, "frame_ms").and_then(|v| v.parse().ok()).filter(|v| [5, 10, 20].contains(v)).unwrap_or(10),
            lossless: get_key(&text, "codec").as_deref() == Some("lossless"),
            keep_local: get_key(&text, "no_redirect").as_deref() == Some("true"),
        }
    }

    /// What the tray and headline say.
    pub fn summary(&self) -> String {
        match self.service {
            ServiceState::NotInstalled => "Sender not installed".into(),
            ServiceState::Stopped => "Sender stopped".into(),
            ServiceState::Running => match self.devices.iter().find(|d| d.state == "active") {
                Some(d) => format!("Sending to {}", d.name),
                None => "Waiting for a receiver".into(),
            },
        }
    }

    fn refresh(&mut self) {
        self.last = Instant::now();
        self.service = service_state();
        if self.service == ServiceState::Running {
            match control(&self.sock, "devices") {
                Ok(t) => self.devices = parse_devices(&t),
                Err(_) => self.devices.clear(), // still starting
            }
        } else {
            self.devices.clear();
            self.code = None;
        }
        if let Some(c) = &self.code {
            if self.devices.len() > c.devices_before {
                let new = self.devices.last().map(|d| d.name.clone()).unwrap_or_default();
                self.message = Some((format!("Paired with {new}"), false));
                self.code = None;
            } else if Instant::now() >= c.until {
                self.code = None;
            }
        }
    }

    /// Call every frame; refreshes at most every two seconds (every half second while a code is showing).
    pub fn tick(&mut self) {
        let every = if self.code.is_some() { Duration::from_millis(500) } else { Duration::from_secs(2) };
        if self.last.elapsed() >= every {
            self.refresh();
        }
    }

    /// Refresh on the next frame (after the service was changed from outside the panel).
    pub fn tick_now(&mut self) {
        self.last = Instant::now() - Duration::from_secs(10);
        self.refresh();
    }

    pub fn is_running(&self) -> bool {
        self.service == ServiceState::Running
    }

    fn act(&mut self, r: Result<(), String>) {
        self.message = r.err().map(|e| (e, true));
        // The agent needs a moment to open its socket; show the new service state at once.
        self.last = Instant::now() - Duration::from_secs(10);
        self.refresh();
    }

    fn open_pairing(&mut self) {
        match control(&self.sock, "pair").map(|t| parse_pairing(&t).ok_or(t)) {
            Ok(Ok((code, link))) => {
                self.code = Some(OpenCode { fingerprint: fingerprint_of_link(&link), code, link, until: Instant::now() + PAIR_WINDOW, devices_before: self.devices.len() });
                self.message = None;
            }
            Ok(Err(other)) => self.message = Some((format!("Unexpected reply from the sender: {other}"), true)),
            Err(e) => self.message = Some((e, true)),
        }
    }

    fn command(&mut self, line: String) {
        self.message = match control(&self.sock, &line) {
            Ok(r) if r.starts_with("error") => Some((r, true)),
            Ok(_) => None,
            Err(e) => Some((e, true)),
        };
        self.last = Instant::now() - Duration::from_secs(10);
        self.refresh();
    }

    fn apply_settings(&mut self) {
        let text = std::fs::read_to_string(&self.config).unwrap_or_default();
        let text = set_key(&text, "kbps", Some(&self.kbps.to_string()));
        let text = set_key(&text, "frame_ms", Some(&self.frame_ms.to_string()));
        let text = set_key(&text, "codec", self.lossless.then_some("lossless"));
        let text = set_key(&text, "no_redirect", self.keep_local.then_some("true"));
        if let Some(parent) = self.config.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&self.config, text) {
            self.message = Some((format!("Saving the settings: {e}"), true));
            return;
        }
        if self.is_running() {
            let r = run_systemctl(&["restart", UNIT]);
            self.act(r);
            if self.message.is_none() {
                self.message = Some(("Settings saved; the sender restarted".into(), false));
            }
        } else {
            self.message = Some(("Settings saved; they apply when the sender starts".into(), false));
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let (title, colour) = match self.service {
            ServiceState::NotInstalled => ("Not set up", MUTED),
            ServiceState::Stopped => ("Stopped", MUTED),
            ServiceState::Running if self.devices.iter().any(|d| d.state == "active") => ("Sending", ACCENT),
            ServiceState::Running => ("Waiting", WARN),
        };
        ui.add_space(4.0);
        ui.label(RichText::new(title).size(34.0).color(colour));
        ui.label(RichText::new(self.summary()).color(MUTED));
        ui.add_space(6.0);

        match self.service {
            ServiceState::NotInstalled => {
                ui.label("The sender runs as a background service so audio keeps flowing when this window is closed. Set it up for your user (a systemd user unit that runs this program).");
                if ui.button("Set up and start").clicked() {
                    let r = install_service(&self.exe);
                    self.act(r);
                }
            }
            ServiceState::Stopped => {
                if ui.button("Start sending").clicked() {
                    self.act(start_service());
                }
            }
            ServiceState::Running => {
                if ui.button("Stop sending").clicked() {
                    self.act(stop_service());
                }
            }
        }
        ui.add_space(6.0);
        ui.separator();

        if self.is_running() {
            self.pairing_ui(ui);
            ui.add_space(4.0);
            ui.separator();
            self.devices_ui(ui);
            ui.add_space(4.0);
            ui.separator();
        }
        self.settings_ui(ui);
        if let Some((m, bad)) = &self.message {
            ui.add_space(6.0);
            ui.label(RichText::new(m).color(if *bad { BAD } else { MUTED }));
        }
    }

    fn pairing_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Add a receiver").color(MUTED));
        match &self.code {
            None => {
                if ui.button("Show pairing code").clicked() {
                    self.open_pairing();
                }
            }
            Some(c) => {
                let left = c.until.saturating_duration_since(Instant::now()).as_secs();
                ui.label(RichText::new(&c.code).monospace().size(40.0).color(ACCENT));
                if !c.fingerprint.is_empty() {
                    ui.label(RichText::new(format!("Key fingerprint {}", c.fingerprint)).monospace().color(ACCENT));
                }
                ui.label(RichText::new(format!("On a phone choose Receive, type the code and tap Pair. On a computer type it in the receive window. Each receiver asks you to confirm the fingerprint above. Valid for {}:{:02}, one use.", left / 60, left % 60)).color(MUTED));
                let link = c.link.clone();
                ui.horizontal(|ui| {
                    if ui.button("Copy link").clicked() {
                        ui.ctx().copy_text(link);
                    }
                    if ui.button("New code").clicked() {
                        self.open_pairing();
                    }
                });
            }
        }
    }

    fn devices_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Paired receivers").color(MUTED));
        if self.devices.is_empty() {
            ui.label(RichText::new("None yet.").color(MUTED));
            return;
        }
        let mut action: Option<String> = None;
        let mut start_rename: Option<(String, String)> = None;
        for d in self.devices.clone() {
            ui.horizontal(|ui| {
                let state = match (d.state.as_str(), d.chosen) {
                    ("active", _) => "playing".to_string(),
                    (s, true) => format!("{s}, chosen"),
                    (s, false) => s.to_string(),
                };
                if self.rename.as_ref().is_some_and(|(k, _)| *k == d.key) {
                    let mut done = false;
                    if let Some((_, text)) = &mut self.rename {
                        let r = ui.add(egui::TextEdit::singleline(text).desired_width(140.0));
                        done = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    }
                    if ui.button("Save").clicked() || done {
                        if let Some((k, t)) = self.rename.take() {
                            let t: String = t.chars().filter(|c| !c.is_control()).collect();
                            if !t.trim().is_empty() {
                                action = Some(format!("rename\t{k}\t{}", t.trim()));
                            }
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        self.rename = None;
                    }
                } else if self.confirm_remove.as_ref() == Some(&d.key) {
                    ui.label(RichText::new(format!("Remove {}? It must pair again with a new code.", d.name)));
                    if ui.button("Remove").clicked() {
                        self.confirm_remove = None;
                        action = Some(format!("remove\t{}", d.key));
                    }
                    if ui.button("Cancel").clicked() {
                        self.confirm_remove = None;
                    }
                } else {
                    ui.label(RichText::new(&d.name).strong());
                    ui.label(RichText::new(state).color(if d.state == "active" { ACCENT } else { MUTED }));
                    if d.state != "active" && ui.small_button("Play here").clicked() {
                        action = Some(format!("use\t{}", d.key));
                    }
                    if ui.small_button("Rename").clicked() {
                        start_rename = Some((d.key.clone(), d.name.clone()));
                    }
                    if ui.small_button("Remove").clicked() {
                        self.confirm_remove = Some(d.key.clone());
                    }
                }
            });
        }
        if start_rename.is_some() {
            self.rename = start_rename;
        }
        if self.devices.iter().any(|d| d.chosen) && ui.small_button("Choose automatically").clicked() {
            action = Some("use\tauto".into());
        }
        if let Some(a) = action {
            self.command(a);
        }
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new(RichText::new("Settings").color(MUTED)).id_salt("send-settings").show(ui, |ui| {
            ui.label(RichText::new("Default quality").color(MUTED));
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.lossless, false, "Opus");
                ui.selectable_value(&mut self.lossless, true, "Lossless PCM");
            });
            ui.label(RichText::new("What is sent until a receiver asks for something else. A receiver's own quality choice always wins.").color(MUTED));
            if self.lossless {
                ui.label(RichText::new("Lossless is about 1.7 Mbit/s in 5 ms packets.").color(MUTED));
            } else {
                egui::CollapsingHeader::new(RichText::new("Opus details").color(MUTED)).id_salt("send-opus").show(ui, |ui| {
                    ui.label(RichText::new("Bitrate").color(MUTED));
                    ui.horizontal(|ui| {
                        ui.add(egui::Slider::new(&mut self.kbps, 64..=256).show_value(false));
                        ui.label(RichText::new(format!("{} kbit/s", self.kbps)).monospace());
                    });
                    ui.label(RichText::new("Frame size").color(MUTED));
                    ui.horizontal(|ui| {
                        for ms in [5u32, 10, 20] {
                            ui.selectable_value(&mut self.frame_ms, ms, format!("{ms} ms"));
                        }
                    });
                });
            }
            ui.checkbox(&mut self.keep_local, "Keep playing on this computer too");
            let label = if self.is_running() { "Save and restart the sender" } else { "Save" };
            if ui.button(label).clicked() {
                self.apply_settings();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_reply_is_parsed() {
        let t = "galaxy               4f91a1bf7ced45be  active\nliving room pc       0123456789abcdef  standby  (chosen)\nold                  ffffffffffffffff  offline";
        let d = parse_devices(t);
        assert_eq!(d.len(), 3);
        assert_eq!((d[0].name.as_str(), d[0].key.as_str(), d[0].state.as_str(), d[0].chosen), ("galaxy", "4f91a1bf7ced45be", "active", false));
        assert_eq!((d[1].name.as_str(), d[1].state.as_str(), d[1].chosen), ("living room pc", "standby", true));
        assert_eq!(d[2].state, "offline");
        assert!(parse_devices("no paired devices yet").is_empty());
        assert!(parse_devices("error: nope").is_empty());
    }

    #[test]
    fn pairing_reply_gives_code_and_link() {
        let t = "pairing code 123456 (valid 2 minutes, one use)\npair: meshwav://pair?host=100.1.2.3&port=47900&pk=ab&code=123456\nor enter in the app: host=100.1.2.3";
        assert_eq!(parse_pairing(t), Some(("123456".into(), "meshwav://pair?host=100.1.2.3&port=47900&pk=ab&code=123456".into())));
        assert_eq!(parse_pairing("error: x"), None);
        let link = format!("meshwav://pair?host=100.1.2.3&port=47900&pk={}&code=123456", "07".repeat(32));
        assert_eq!(fingerprint_of_link(&link), "DBA9-8DD2-C3ED");
        assert_eq!(fingerprint_of_link("meshwav://pair?host=100.1.2.3&port=47900&pk=ab&code=123456"), "");
        assert_eq!(parse_pairing("pairing code abcdef\npair: x"), None);
    }

    #[test]
    fn settings_edit_keeps_other_lines() {
        let t = "# my notes\nkbps = 128  # low\nsource = \"x.monitor\"\n";
        let t = set_key(t, "kbps", Some("200"));
        let t = set_key(&t, "frame_ms", Some("20"));
        let t = set_key(&t, "no_redirect", Some("true"));
        assert_eq!(t, "# my notes\nkbps = 200\nsource = \"x.monitor\"\nframe_ms = 20\nno_redirect = true\n");
        let t = set_key(&t, "no_redirect", None);
        assert!(!t.contains("no_redirect") && t.contains("source = \"x.monitor\""));
        assert_eq!(get_key(&t, "source").as_deref(), Some("x.monitor"));
        assert_eq!(get_key(&t, "kbps").as_deref(), Some("200"));
        assert_eq!(get_key(&t, "bind"), None);
        // a commented-out key is not touched
        assert_eq!(set_key("# kbps = 1\n", "kbps", Some("9")), "# kbps = 1\nkbps = 9\n");
    }

    #[test]
    fn unit_points_at_this_program() {
        let u = unit_text(Path::new("/opt/mw/meshwav"));
        assert!(u.contains("ExecStart=/opt/mw/meshwav send\n"));
        assert!(!u.contains("ExecStart=/usr/bin/meshwav send"));
    }

    #[test]
    fn control_talks_to_a_socket() {
        let dir = std::env::temp_dir().join(format!("mw-gui-sock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c.sock");
        let l = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut line = String::new();
            std::io::BufRead::read_line(&mut std::io::BufReader::new(&s), &mut line).unwrap();
            writeln!(s, "got {}", line.trim()).unwrap();
        });
        assert_eq!(control(&path, "use\tabc").unwrap(), "got use\tabc");
        h.join().unwrap();
        assert!(control(&dir.join("none.sock"), "x").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
