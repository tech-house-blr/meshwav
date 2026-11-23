//! The receiving side's command line, shared by `meshwav-receiver` and `meshwav receive`.
use crate::sink::{Pacat, RawFile, Sink};
use crate::store::{self, Paired};
use crate::{pair, run, Settings, Status};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

const USAGE_TEMPLATE: &str = "{PROG}: play a Meshwav sender's audio on this computer

usage:
  {PROG} pair CODE [--name NAME]
      Pair once. Type the 6-digit code the sender is showing; the sender is found on your tailnet.
      A meshwav://pair link works too.
  {PROG} run [--latency-ms N] [--device SINK]
  {PROG} outputs
      Connect to the paired agent and play. Reconnects by itself. Ctrl-C stops and hands the agent's audio back.
  {PROG} forget
      Forget the paired agent.

options:
  --dir PATH         settings folder (default ~/.config/meshwav/receiver)
  --name NAME        device name shown in the agent's list (default: this computer's host name)
  --device SINK      play to this sound-card output instead of the default (names from `outputs`)
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
    let cmd = args.first().cloned().unwrap_or_default();
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
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
                crate::discover::pair_with_code(arg.trim(), identity(), name, &STOP)
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
        "run" => {
            let p = store::load_paired(&dir).unwrap_or_else(|e| die(e)).unwrap_or_else(|| die(format!("not paired yet; see `{} pair`", prog())));
            let s = Settings { agent: p.agent, agent_key: p.key, identity: identity(), name };
            let sink: Box<dyn Sink> = match &output {
                Some(f) => Box::new(RawFile::create(f).unwrap_or_else(|e| die(format!("--output: {e}")))),
                None => Box::new(Pacat::start(latency, device.as_deref()).unwrap_or_else(|e| die(e))),
            };
            let status = Status::default();
            let mut last = String::new();
            let result = std::thread::scope(|sc| {
                let h = sc.spawn(|| run(&s, sink, &STOP, &status));
                while !h.is_finished() {
                    std::thread::sleep(Duration::from_millis(500));
                    let st = status.snapshot();
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
