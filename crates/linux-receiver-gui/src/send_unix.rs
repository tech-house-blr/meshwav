use super::ServiceState;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub const UNIT: &str = "meshwav-agent";
pub const SETUP_HELP: &str = "The sender runs as a background service so audio keeps flowing when this window is closed. Set it up for your user (a systemd user unit that runs this program).";

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


pub fn restart_service() -> Result<(), String> {
    run_systemctl(&["restart", UNIT])
}
