#![cfg_attr(windows, windows_subsystem = "windows")]
//! meshwav: one program for both directions. `send` is the former agent, `receive` the former receiver.

const USAGE: &str = "meshwav: private low-latency audio over your tailnet

usage:
  meshwav                       open the window (Send and Receive)
  meshwav send [options]        send this computer's audio (runs until stopped)
  meshwav receive <command>     play audio sent by another device
      pair CODE | run | outputs | forget
  meshwav pair                  open the pairing window on the running sender
  meshwav pair CODE             pair this computer as a receiver with the code a sender shows
  meshwav devices               list receivers paired with the running sender
  meshwav use <device|auto>     choose which receiver plays
  meshwav rename <device> <new name>
  meshwav remove <device>

`meshwav send --help` and `meshwav receive help` list each side's options.
";

/// A Windows-subsystem program has no console of its own, so no window flashes up on double-click.
/// Started from a terminal, it joins that terminal so `meshwav send` and friends still print and read.
#[cfg(windows)]
fn attach_parent_console() {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Console::{
        AttachConsole, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
            // Started from Explorer, the login startup or the window: no console and nowhere to print,
            // so keep the output in a log file for diagnosing the background sender.
            let dir = std::env::var_os("APPDATA").map(|a| std::path::PathBuf::from(a).join("meshwav"));
            if let Some(dir) = dir {
                let _ = std::fs::create_dir_all(&dir);
                let log = dir.join("agent.log");
                // start over when it has grown large, so it never fills the disk
                if std::fs::metadata(&log).map(|m| m.len() > 1_000_000).unwrap_or(false) {
                    let _ = std::fs::remove_file(&log);
                }
                if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(&log) {
                    let h = HANDLE(f.as_raw_handle() as isize);
                    let _ = SetStdHandle(STD_OUTPUT_HANDLE, h);
                    let _ = SetStdHandle(STD_ERROR_HANDLE, h);
                    std::mem::forget(f);
                }
            }
            return;
        }
        // A handle the parent redirected (to a file or pipe) is already valid and is kept as it is.
        for (id, name, write) in [(STD_INPUT_HANDLE, "CONIN$", false), (STD_OUTPUT_HANDLE, "CONOUT$", true), (STD_ERROR_HANDLE, "CONOUT$", true)] {
            let missing = GetStdHandle(id).map(|h| h.is_invalid() || h.0 == 0).unwrap_or(true);
            if missing {
                if let Ok(f) = std::fs::OpenOptions::new().read(!write).write(write).open(name) {
                    let _ = SetStdHandle(id, HANDLE(f.as_raw_handle() as isize));
                    std::mem::forget(f);
                }
            }
        }
    }
}

fn main() {
    #[cfg(windows)]
    attach_parent_console();
    let args: Vec<String> = std::env::args().collect();
    // No command, or a meshwav://pair link handed over by the desktop: open the window.
    if args.len() == 1 || args.get(1).is_some_and(|a| a.starts_with("meshwav://")) {
        if let Err(e) = meshwav_receiver_gui::run_window() {
            eprintln!("meshwav: cannot open the window: {e}");
            std::process::exit(1);
        }
        return;
    }
    let rest = |n: usize| args.iter().skip(n).cloned().collect::<Vec<_>>();
    match args.get(1).map(String::as_str) {
        Some("send") => meshwav_agent::run([vec!["meshwav send".to_string()], rest(2)].concat()),
        Some("receive") => meshwav_receiver::cli::run_cli("meshwav receive", rest(2)),
        // `pair` with no argument talks to the running sender; with a code it pairs this receiver.
        Some("pair") if args.len() > 2 && !args[2].starts_with("--") => meshwav_receiver::cli::run_cli("meshwav", rest(1)),
        Some("pair" | "devices" | "use" | "rename" | "remove") => meshwav_agent::run([vec!["meshwav".to_string()], rest(1)].concat()),
        Some("outputs") => meshwav_receiver::cli::run_cli("meshwav", rest(1)),
        None | Some("help" | "--help" | "-h") => print!("{USAGE}"),
        Some(other) => {
            eprintln!("meshwav: unknown command `{other}`\n\n{USAGE}");
            std::process::exit(2);
        }
    }
}
