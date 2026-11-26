//! The receiving side's command line, shared by `meshwav-receiver` and `meshwav receive`.
use audio_protocol::handshake;
use crate::sink::{RawFile, Sink};
use crate::store::{self, Paired};
use crate::{pair, run, Settings, Status};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static STOP: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

const USAGE_TEMPLATE: &str = "{PROG}: play a Meshwav sender's audio on this computer

usage:
  {PROG} pair CODE [--name NAME] [--fingerprint FP]
      Pair once. Type the 6-digit code the sender is showing; the sender is found on your tailnet.
      You are asked to confirm the sender's key fingerprint against the one on its screen, or pass it
      as --fingerprint 4F2A-91C0-7B3E. A meshwav://pair link works too and needs no confirmation.
  {PROG} run [--latency-ms N] [--device SINK] [--lossless]
      Connect to the paired agent and play. Reconnects by itself. Ctrl-C stops and hands the agent's audio back.
  {PROG} outputs
      List the sound outputs that `--device` accepts.
  {PROG} tone [--device SINK] [--latency-ms N]
      Play a quiet 440 Hz tone for two seconds, to check that this computer's sound output works.
  {PROG} forget
      Forget the paired agent.

options:
  --dir PATH         settings folder (default ~/.config/meshwav/receiver)
  --fingerprint FP   with `pair CODE`: pair only with the sender whose key fingerprint is FP
  --name NAME        device name shown in the agent's list (default: this computer's host name)
  --device SINK      play to this sound-card output instead of the default (names from `outputs`)
  --lossless         with `run`: ask the sender for uncompressed audio (about 1.7 Mbit/s) instead of Opus
  --latency-ms N     output buffer asked of the sound server, 10-500 (default 40)
  --output FILE      with `run`: write raw s16le 48 kHz stereo to FILE (`-` = stdout) instead of playing
";

static PROG: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn prog() -> &'static str {
    PROG.get().map(String::as_str).unwrap_or("meshwav-receiver")
}

fn usage() -> String {
    USAGE_TEMPLATE.replace("{PROG}", prog())
}

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("{}: {msg}", prog());
    std::process::exit(1)
}

/// `program` is the name used in messages (`meshwav-receiver` or `meshwav receive`); `args` excludes it.
pub fn run_cli(program: &str, args: Vec<String>) {
    let _ = PROG.set(program.to_string());
    let mut args = args;
    let lossless = match args.iter().position(|a| a == "--lossless") {
        Some(i) => {
            args.remove(i);
            true
        }
        None => false,
    };
    let mut flag = |name: &str| -> Option<String> {
        let i = args.iter().position(|a| a == name)?;
        if i + 1 >= args.len() {
            die(format!("{name} needs a value"));
        }
        args.remove(i);
        Some(args.remove(i))
    };
    let dir = flag("--dir").map(PathBuf::from).unwrap_or_else(store::default_dir);
    let name = flag("--name").unwrap_or_else(store::host_name);
    let latency: u32 = match flag("--latency-ms") {
        None => 40,
        Some(v) => match v.parse() {
            Ok(n) if (10..=500).contains(&n) => n,
            _ => die(format!("--latency-ms: `{v}` is not a number from 10 to 500")),
        },
    };
    let output = flag("--output");
    let device = flag("--device");
    let want = flag("--fingerprint").map(|f| handshake::normalize_fingerprint(&f).unwrap_or_else(|| die("--fingerprint must be 12 hex digits like 4F2A-91C0-7B3E")));
    let cmd = args.first().cloned().unwrap_or_default();
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
    }
    #[cfg(windows)]
    {
        let _ = ctrlc::set_handler(|| STOP.store(true, Ordering::Relaxed));
    }
    let identity = || store::load_or_create_identity(&dir).unwrap_or_else(|e| die(format!("identity: {e}")));
    match cmd.as_str() {
        "pair" => {
            let arg = args.get(1).unwrap_or_else(|| die("pair needs the 6-digit code shown by the sender (or a meshwav://pair link)"));
            let paired = if arg.contains("meshwav://") {
                let l = store::parse_pair_link(arg).unwrap_or_else(|e| die(e));
                let agent = store::resolve(&l).unwrap_or_else(|e| die(e));
                let s = Settings { agent, agent_key: l.key, identity: identity(), name };
                eprintln!("pairing with {agent} ...");
                pair(&s, &l.code, &STOP).map(|_| Paired { agent, key: l.key })
            } else {
                eprintln!("looking for the device showing code {arg} ...");
                let want = want.clone();
                crate::discover::pair_with_code(arg.trim(), identity(), name, &STOP, &mut |f| confirm_sender(f, want.as_deref()))
            };
            let p = paired.unwrap_or_else(|e| die(e));
            store::save_paired(&dir, &p).unwrap_or_else(|e| die(format!("saving the pairing: {e}")));
            println!("paired with {}. Start playing with: {} run", p.agent, prog());
        }
        "outputs" => {
            for o in crate::sink::list_outputs() {
                println!("{}\t{}", o.name, o.description);
            }
        }
        "tone" => {
            let mut sink = crate::sink::open_default(latency, device.as_deref()).unwrap_or_else(|e| die(e));
            let frame: Vec<i16> = (0..480).flat_map(|i| {
                let v = ((i as f32 / 48.0 * std::f32::consts::TAU * 0.44).sin() * 3000.0) as i16;
                [v, v]
            }).collect();
            for _ in 0..200 {
                sink.write(&frame).unwrap_or_else(|e| die(format!("playing: {e}")));
            }
            std::thread::sleep(Duration::from_millis(300));
            println!("played 2 s of 440 Hz");
        }
        "run" => {
            let p = store::load_paired(&dir).unwrap_or_else(|e| die(e)).unwrap_or_else(|| die(format!("not paired yet; see `{} pair`", prog())));
            let s = Settings { agent: p.agent, agent_key: p.key, identity: identity(), name };
            let sink: Box<dyn Sink> = match &output {
                Some(f) => Box::new(RawFile::create(f).unwrap_or_else(|e| die(format!("--output: {e}")))),
                None => crate::sink::open_default(latency, device.as_deref()).unwrap_or_else(|e| die(e)),
            };
            let status = Status::default();
            status.set_lossless(lossless);
            let mut last = String::new();
            let result = std::thread::scope(|sc| {
                let h = sc.spawn(|| run(&s, sink, &STOP, &status));
                while !h.is_finished() {
                    std::thread::sleep(Duration::from_millis(500));
                    let st = status.snapshot();
                    store::remember_agent_name(&dir, &st.sender_name);
                    if st.state != last {
                        eprintln!("{}", st.state);
                        last = st.state;
                    }
                }
                h.join().unwrap()
            });
            if let Err(e) = result {
                die(e);
            }
        }
        "forget" => match store::forget(&dir) {
            Ok(true) => println!("forgotten"),
            Ok(false) => println!("nothing was paired"),
            Err(e) => die(e),
        },
        "" | "help" | "--help" | "-h" => print!("{}", usage()),
        other => die(format!("unknown command `{other}`\n\n{}", usage())),
    }
}

/// Whether to pair with a sender found by code alone. With `--fingerprint` the answer is automatic
/// (it must be the sender's own fingerprint); otherwise a person is asked, and without a terminal the
/// answer is no, so a script can never pair with an unverified sender by accident.
fn confirm_sender(f: &crate::discover::Found, want: Option<&str>) -> bool {
    use std::io::{BufRead, IsTerminal, Write};
    let fp = f.fingerprint();
    if let Some(w) = want {
        if fp == w {
            return true;
        }
        eprintln!("skipping {} (key fingerprint {fp} is not {w})", f.addr);
        return false;
    }
    if !std::io::stdin().is_terminal() {
        eprintln!("found {} with key fingerprint {fp}; not pairing without confirmation (pass --fingerprint {fp} if it matches the sender's screen)", f.addr);
        return false;
    }
    eprint!("Found a sender at {} with key fingerprint {fp}.\nDoes that match the fingerprint on the sender's screen? [y/N] ", f.addr);
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).is_ok() && matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::Found;

    fn found(k: u8) -> Found {
        Found { addr: "100.64.0.9:47900".parse().unwrap(), key: [k; 32] }
    }

    #[test]
    fn a_given_fingerprint_accepts_only_that_sender() {
        let f = found(7);
        assert!(confirm_sender(&f, Some("DBA9-8DD2-C3ED")));
        assert!(!confirm_sender(&found(8), Some("DBA9-8DD2-C3ED")));
    }
}
