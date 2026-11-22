//! Audio redirection: while a receiver is connected, the PC's default output becomes a virtual
//! "Meshwav" sink, every playing stream is moved onto it and the agent captures only that sink.
//! Local outputs (IEMs, speakers, HDMI) go quiet and can come and go without touching the stream.
//! When the receiver is gone (after a grace period) or the agent exits, the previous default is
//! restored and the streams are moved back. The physical sinks' volume and mute are never touched.
//!
//! All sound-server access goes through [`Audio`], so the policy below is tested against a fake.
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

pub const SINK_NAME: &str = "meshwav";
pub const MONITOR: &str = "meshwav.monitor";
/// Network blips shorter than this keep the redirect in place.
pub const GRACE: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, PartialEq)]
pub struct Input {
    pub id: u32,
    pub sink: String,
}

pub trait Audio {
    fn default_sink(&mut self) -> Option<String>;
    fn sinks(&mut self) -> Vec<String>;
    fn sink_inputs(&mut self) -> Vec<Input>;
    fn set_default(&mut self, sink: &str) -> bool;
    fn move_input(&mut self, id: u32, sink: &str) -> bool;
    /// Create the virtual sink; Ok if it already exists.
    fn ensure_sink(&mut self) -> Result<(), String>;
    fn remove_sink(&mut self);
}

/// The real thing, through `pactl` (PulseAudio or PipeWire-pulse).
pub struct Pactl;

fn pactl(args: &[&str]) -> Option<String> {
    let o = Command::new("pactl").args(args).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

fn columns(line: &str) -> Vec<&str> {
    line.split('\t').collect()
}

impl Audio for Pactl {
    fn default_sink(&mut self) -> Option<String> {
        pactl(&["get-default-sink"]).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }
    fn sinks(&mut self) -> Vec<String> {
        pactl(&["list", "short", "sinks"]).unwrap_or_default().lines().filter_map(|l| columns(l).get(1).map(|s| s.to_string())).collect()
    }
    fn sink_inputs(&mut self) -> Vec<Input> {
        let ids: Vec<(String, String)> =
            pactl(&["list", "short", "sinks"]).unwrap_or_default().lines().filter_map(|l| { let c = columns(l); Some((c.first()?.to_string(), c.get(1)?.to_string())) }).collect();
        pactl(&["list", "short", "sink-inputs"])
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let c = columns(l);
                let id = c.first()?.parse().ok()?;
                let sink = ids.iter().find(|(i, _)| Some(i.as_str()) == c.get(1).copied())?.1.clone();
                Some(Input { id, sink })
            })
            .collect()
    }
    fn set_default(&mut self, sink: &str) -> bool {
        pactl(&["set-default-sink", sink]).is_some()
    }
    fn move_input(&mut self, id: u32, sink: &str) -> bool {
        pactl(&["move-sink-input", &id.to_string(), sink]).is_some()
    }
    fn ensure_sink(&mut self) -> Result<(), String> {
        if self.sinks().iter().any(|s| s == SINK_NAME) {
            return Ok(());
        }
        let name = format!("sink_name={SINK_NAME}");
        let props = "sink_properties=device.description=Meshwav";
        pactl(&["load-module", "module-null-sink", &name, props, "channels=2", "rate=48000"]).map(|_| ()).ok_or_else(|| "pactl load-module module-null-sink failed".to_string())
    }
    fn remove_sink(&mut self) {
        let Some(list) = pactl(&["list", "short", "modules"]) else { return };
        for l in list.lines() {
            let c = columns(l);
            if c.get(1) == Some(&"module-null-sink") && c.get(2).is_some_and(|a| a.split_whitespace().any(|w| w == format!("sink_name={SINK_NAME}"))) {
                if let Some(id) = c.first() {
                    let _ = pactl(&["unload-module", id]);
                }
            }
        }
    }
}

/// Redirect policy. `tick` is called every few hundred ms with whether a receiver is connected.
pub struct Redirect<A: Audio> {
    audio: A,
    state_file: Option<PathBuf>,
    /// The output to give back; Some while the redirect is engaged.
    prev: Option<String>,
    gone_since: Option<Instant>,
}

impl<A: Audio> Redirect<A> {
    pub fn new(audio: A, state_file: Option<PathBuf>) -> Self {
        Redirect { audio, state_file, prev: None, gone_since: None }
    }

    #[allow(dead_code)]
    pub fn engaged(&self) -> bool {
        self.prev.is_some()
    }

    /// Create the sink, and undo what a previous run that died left behind.
    pub fn start(&mut self) -> Result<(), String> {
        if let Some(prev) = self.state_file.as_deref().and_then(read_state) {
            self.prev = Some(prev);
            println!("redirect: restoring the output a previous run left redirected");
            self.release();
        }
        self.audio.ensure_sink()
    }

    /// A real output to give back: the saved one if it still exists, else any other sink.
    fn pick_restore(&mut self, wanted: Option<&str>) -> Option<String> {
        let sinks = self.audio.sinks();
        wanted.filter(|w| *w != SINK_NAME && sinks.iter().any(|s| s == w)).map(str::to_string).or_else(|| sinks.into_iter().find(|s| s != SINK_NAME))
    }

    pub fn tick(&mut self, connected: bool, now: Instant) {
        if connected {
            self.gone_since = None;
            if self.prev.is_none() {
                self.engage();
            } else {
                self.maintain();
            }
        } else if self.prev.is_some() {
            let since = *self.gone_since.get_or_insert(now);
            if now.duration_since(since) >= GRACE {
                self.release();
            }
        }
    }

    fn engage(&mut self) {
        if self.audio.ensure_sink().is_err() {
            return; // try again next tick
        }
        let cur = self.audio.default_sink();
        let prev = match cur {
            Some(c) if c != SINK_NAME => Some(c),
            _ => self.pick_restore(None), // default already ours (stale): fall back to any real output
        };
        let Some(prev) = prev else {
            eprintln!("redirect: no real output sink to hand back later; not redirecting");
            return;
        };
        if !self.audio.set_default(SINK_NAME) {
            return;
        }
        if let Some(f) = &self.state_file {
            write_state(f, &prev);
        }
        println!("redirect: all audio now goes to the receiver (was {prev})");
        self.prev = Some(prev);
        self.move_all_to(SINK_NAME);
    }

    /// While engaged: keep the default ours (a plugged-in device may grab it; remember it as the new
    /// output to give back) and pull in streams that were started on a device or reconnected to one.
    fn maintain(&mut self) {
        match self.audio.default_sink() {
            Some(c) if c != SINK_NAME => {
                println!("redirect: default output changed to {c}; keeping the redirect");
                self.prev = Some(c.clone());
                if let Some(f) = &self.state_file {
                    write_state(f, &c);
                }
                self.audio.set_default(SINK_NAME);
            }
            _ => {}
        }
        self.move_all_to(SINK_NAME);
    }

    fn move_all_to(&mut self, sink: &str) {
        for i in self.audio.sink_inputs() {
            if i.sink != sink {
                self.audio.move_input(i.id, sink);
            }
        }
    }

    /// Give the output back. Safe to call when not engaged.
    pub fn release(&mut self) {
        let Some(prev) = self.prev.take() else { return };
        self.gone_since = None;
        let target = self.pick_restore(Some(&prev));
        match &target {
            Some(t) => {
                self.audio.set_default(t);
                for i in self.audio.sink_inputs() {
                    if i.sink == SINK_NAME {
                        self.audio.move_input(i.id, t);
                    }
                }
                println!("redirect: audio back on {t}");
            }
            None => eprintln!("redirect: no output sink left to restore"),
        }
        if let Some(f) = &self.state_file {
            let _ = std::fs::remove_file(f);
        }
    }

    /// Release and remove the virtual sink (agent exit).
    pub fn shutdown(&mut self) {
        self.release();
        self.audio.remove_sink();
    }
}

fn read_state(p: &Path) -> Option<String> {
    let s = std::fs::read_to_string(p).ok()?;
    let name = s.trim();
    (!name.is_empty() && !name.contains(['\n', '\0'])).then(|| name.to_string())
}

fn write_state(p: &Path, prev: &str) {
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(p, prev);
}

#[cfg(test)]
pub mod fake {
    use super::*;

    /// An in-memory sound server.
    #[derive(Default, Clone)]
    pub struct Fake {
        pub sinks: Vec<String>,
        pub default: Option<String>,
        pub inputs: Vec<Input>,
        pub removed: bool,
    }

    impl Audio for Fake {
        fn default_sink(&mut self) -> Option<String> { self.default.clone() }
        fn sinks(&mut self) -> Vec<String> { self.sinks.clone() }
        fn sink_inputs(&mut self) -> Vec<Input> { self.inputs.clone() }
        fn set_default(&mut self, s: &str) -> bool {
            if self.sinks.iter().any(|x| x == s) { self.default = Some(s.into()); true } else { false }
        }
        fn move_input(&mut self, id: u32, s: &str) -> bool {
            if !self.sinks.iter().any(|x| x == s) { return false; }
            if let Some(i) = self.inputs.iter_mut().find(|i| i.id == id) { i.sink = s.into(); true } else { false }
        }
        fn ensure_sink(&mut self) -> Result<(), String> {
            if !self.sinks.iter().any(|x| x == SINK_NAME) { self.sinks.push(SINK_NAME.into()); }
            self.removed = false;
            Ok(())
        }
        fn remove_sink(&mut self) {
            self.sinks.retain(|x| x != SINK_NAME);
            self.removed = true;
            if self.default.as_deref() == Some(SINK_NAME) { self.default = None; }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::Fake;
    use super::*;

    fn iems() -> Fake {
        Fake {
            sinks: vec!["iems".into(), "hdmi".into()],
            default: Some("iems".into()),
            inputs: vec![Input { id: 1, sink: "iems".into() }, Input { id: 2, sink: "hdmi".into() }],
            removed: false,
        }
    }

    fn rd(f: Fake) -> Redirect<Fake> {
        Redirect::new(f, None)
    }

    fn sink_of(r: &Redirect<Fake>, id: u32) -> String {
        r.audio.inputs.iter().find(|i| i.id == id).unwrap().sink.clone()
    }

    #[test]
    fn connect_makes_the_virtual_sink_the_default_and_moves_every_stream() {
        let mut r = rd(iems());
        r.start().unwrap();
        assert_eq!(r.audio.default.as_deref(), Some("iems"), "creating the sink changes nothing audible");
        r.tick(true, Instant::now());
        assert!(r.engaged());
        assert_eq!(r.audio.default.as_deref(), Some(SINK_NAME));
        assert_eq!((sink_of(&r, 1), sink_of(&r, 2)), (SINK_NAME.into(), SINK_NAME.into()));
    }

    #[test]
    fn a_new_stream_started_on_a_device_is_pulled_in() {
        let mut r = rd(iems());
        r.start().unwrap();
        let t = Instant::now();
        r.tick(true, t);
        r.audio.inputs.push(Input { id: 9, sink: "hdmi".into() });
        r.tick(true, t);
        assert_eq!(sink_of(&r, 9), SINK_NAME);
    }

    #[test]
    fn short_drop_keeps_the_redirect_and_long_one_restores_it() {
        let mut r = rd(iems());
        r.start().unwrap();
        let t = Instant::now();
        r.tick(true, t);
        r.tick(false, t + Duration::from_secs(5));
        r.tick(false, t + Duration::from_secs(10));
        assert!(r.engaged(), "within the grace period");
        r.tick(true, t + Duration::from_secs(11)); // back: grace timer resets
        r.tick(false, t + Duration::from_secs(12));
        r.tick(false, t + Duration::from_secs(12) + GRACE - Duration::from_millis(1));
        assert!(r.engaged());
        r.tick(false, t + Duration::from_secs(12) + GRACE);
        assert!(!r.engaged());
        assert_eq!(r.audio.default.as_deref(), Some("iems"));
        assert_eq!((sink_of(&r, 1), sink_of(&r, 2)), ("iems".into(), "iems".into()));
    }

    #[test]
    fn unplugging_the_iems_while_redirected_changes_nothing_and_restore_picks_what_exists() {
        let mut r = rd(iems());
        r.start().unwrap();
        let t = Instant::now();
        r.tick(true, t);
        r.audio.sinks.retain(|s| s != "iems"); // unplug
        r.tick(true, t);
        assert_eq!(r.audio.default.as_deref(), Some(SINK_NAME));
        r.tick(false, t);
        r.tick(false, t + GRACE);
        assert_eq!(r.audio.default.as_deref(), Some("hdmi"), "the saved output is gone, so fall back to another real one");
        assert_eq!(sink_of(&r, 1), "hdmi");
    }

    #[test]
    fn a_device_that_grabs_the_default_is_remembered_and_the_redirect_kept() {
        let mut r = rd(iems());
        r.start().unwrap();
        let t = Instant::now();
        r.tick(true, t);
        r.audio.sinks.push("usb-dac".into());
        r.audio.default = Some("usb-dac".into()); // the session manager prefers the new device
        r.tick(true, t);
        assert_eq!(r.audio.default.as_deref(), Some(SINK_NAME));
        r.release();
        assert_eq!(r.audio.default.as_deref(), Some("usb-dac"), "the newest real output is what comes back");
    }

    #[test]
    fn shutdown_restores_and_removes_the_sink_and_is_repeatable() {
        let mut r = rd(iems());
        r.start().unwrap();
        r.tick(true, Instant::now());
        r.shutdown();
        r.shutdown();
        assert!(r.audio.removed && !r.audio.sinks.iter().any(|s| s == SINK_NAME));
        assert_eq!(r.audio.default.as_deref(), Some("iems"));
        assert_eq!((sink_of(&r, 1), sink_of(&r, 2)), ("iems".into(), "iems".into()));
    }

    #[test]
    fn never_connected_means_nothing_changes() {
        let mut r = rd(iems());
        r.start().unwrap();
        for s in 0..60 {
            r.tick(false, Instant::now() + Duration::from_secs(s));
        }
        assert!(!r.engaged());
        assert_eq!(r.audio.default.as_deref(), Some("iems"));
        assert_eq!((sink_of(&r, 1), sink_of(&r, 2)), ("iems".into(), "hdmi".into()));
    }

    #[test]
    fn a_crashed_run_is_undone_from_the_state_file() {
        let dir = std::env::temp_dir().join(format!("mw-redirect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let state = dir.join("redirect.state");
        let mut r = Redirect::new(iems(), Some(state.clone()));
        r.start().unwrap();
        r.tick(true, Instant::now());
        assert_eq!(std::fs::read_to_string(&state).unwrap(), "iems");
        let crashed = r.audio.clone(); // process died here: server keeps the state, the memory is gone
        let mut r2 = Redirect::new(crashed, Some(state.clone()));
        r2.start().unwrap();
        assert_eq!(r2.audio.default.as_deref(), Some("iems"));
        assert_eq!(sink_of(&r2, 1), "iems");
        assert!(!state.exists());
        assert!(!r2.engaged());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_default_without_a_state_file_falls_back_to_a_real_sink() {
        let mut f = iems();
        f.sinks.push(SINK_NAME.into());
        f.default = Some(SINK_NAME.into());
        let mut r = rd(f);
        r.start().unwrap();
        r.tick(true, Instant::now());
        r.release();
        assert_eq!(r.audio.default.as_deref(), Some("iems"));
    }

    #[test]
    fn no_real_sink_means_no_redirect() {
        let mut r = rd(Fake { sinks: vec![], default: None, inputs: vec![], removed: false });
        r.start().unwrap();
        r.tick(true, Instant::now());
        assert!(!r.engaged());
    }

    #[test]
    fn corrupt_state_files_are_ignored() {
        let dir = std::env::temp_dir().join(format!("mw-redirect-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("s");
        std::fs::write(&p, "a\nb").unwrap();
        assert_eq!(read_state(&p), None);
        std::fs::write(&p, "  \n").unwrap();
        assert_eq!(read_state(&p), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
