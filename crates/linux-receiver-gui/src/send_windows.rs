//! The Windows side of the Send panel. The sender is this same program run as `meshwav.exe send`,
//! started hidden and detached so audio keeps flowing when the window is closed. "Set up" adds it to
//! the current user's login startup (the `Run` key) and starts it; the panel talks to it over the
//! agent's loopback control port, using the token in its control file.
use super::ServiceState;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpStream};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

pub const SETUP_HELP: &str = "The sender runs in the background so audio keeps flowing when this window is closed. Set it up to start when you sign in to Windows.";

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "MeshwavSender";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const DETACHED_PROCESS: u32 = 0x0000_0008;

fn control_file() -> PathBuf {
    PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("meshwav").join("control.sock")
}

fn connect(file: &Path) -> Result<TcpStream, String> {
    let info = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
    let (port, token) = info.trim().split_once(' ').ok_or("the sender's control file is malformed")?;
    let port: u16 = port.parse().map_err(|_| "the sender's control file has a bad port")?;
    let mut s = TcpStream::connect_timeout(&(Ipv4Addr::LOCALHOST, port).into(), Duration::from_secs(1)).map_err(|e| e.to_string())?;
    writeln!(s, "{token}").map_err(|e| e.to_string())?;
    Ok(s)
}

/// One request to the running agent. `sock` is the control file.
pub fn control(sock: &Path, line: &str) -> Result<String, String> {
    let mut s = connect(sock).map_err(|e| format!("cannot reach the sender ({e})"))?;
    let t = Some(Duration::from_secs(2));
    let _ = s.set_read_timeout(t);
    let _ = s.set_write_timeout(t);
    writeln!(s, "{line}").map_err(|e| e.to_string())?;
    let mut out = String::new();
    s.read_to_string(&mut out).map_err(|e| e.to_string())?;
    Ok(out.trim_end().to_string())
}

/// The sender program: this very executable, which runs the sender when given `send`.
fn agent_exe() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| e.to_string())
}

fn reg(args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("reg").args(args).creation_flags(CREATE_NO_WINDOW).output().map_err(|e| format!("reg: {e}"))
}

fn running() -> bool {
    connect(&control_file()).is_ok()
}

pub fn service_state() -> ServiceState {
    if running() {
        ServiceState::Running
    } else if reg(&["query", RUN_KEY, "/v", RUN_VALUE]).map(|o| o.status.success()).unwrap_or(false) {
        ServiceState::Stopped
    } else {
        ServiceState::NotInstalled
    }
}

pub fn start_service() -> Result<(), String> {
    if running() {
        return Ok(());
    }
    let agent = agent_exe()?;
    Command::new(agent)
        .arg("send")
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("starting the sender: {e}"))?;
    Ok(())
}

pub fn stop_service() -> Result<(), String> {
    if !running() {
        return Ok(());
    }
    control(&control_file(), "quit")?;
    // Wait for it to hand the volume back and exit.
    for _ in 0..30 {
        if !running() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("the sender did not stop within 3 seconds".into())
}

pub fn restart_service() -> Result<(), String> {
    stop_service()?;
    start_service()
}

/// Add the sender to this user's login startup, then start it. `exe` (this program) is not used:
/// the sender is its own program.
pub fn install_service(_exe: &Path) -> Result<(), String> {
    let agent = agent_exe()?;
    let value = format!("\"{}\" send", agent.display());
    let o = reg(&["add", RUN_KEY, "/v", RUN_VALUE, "/t", "REG_SZ", "/d", &value, "/f"])?;
    if !o.status.success() {
        return Err(format!("could not add the login startup entry: {}", String::from_utf8_lossy(&o.stderr).trim()));
    }
    start_service()
}
