//! meshwav-receiver: play a Meshwav agent's audio on this computer.
use meshwav_receiver::sink::{Pacat, RawFile, Sink};
use meshwav_receiver::store::{self, Paired};
use meshwav_receiver::{pair, run, Settings, Status};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

const USAGE: &str = "meshwav-receiver: play a Meshwav agent's audio on this computer

usage:
  meshwav-receiver pair 'meshwav://pair?host=...&port=...&pk=...&code=123456' [--name NAME]
      Pair with an agent once. On the agent's machine run `meshwav-agent pair` and pass its `pair:` link here.
  meshwav-receiver run [--latency-ms N]
      Connect to the paired agent and play. Reconnects by itself. Ctrl-C stops and hands the agent's audio back.
  meshwav-receiver forget
      Forget the paired agent.

options:
  --dir PATH         settings folder (default ~/.config/meshwav-receiver)
  --name NAME        device name shown in the agent's list (default: this computer's host name)
  --latency-ms N     output buffer asked of the sound server, 10-500 (default 40)
  --output FILE      with `run`: write raw s16le 48 kHz stereo to FILE (`-` = stdout) instead of playing
";

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("meshwav-receiver: {msg}");
    std::process::exit(1)
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
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
    let cmd = args.first().cloned().unwrap_or_default();
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
    }
    let identity = || store::load_or_create_identity(&dir).unwrap_or_else(|e| die(format!("identity: {e}")));
    match cmd.as_str() {
        "pair" => {
            let link = args.get(1).unwrap_or_else(|| die("pair needs the agent's pair: link"));
            let l = store::parse_pair_link(link).unwrap_or_else(|e| die(e));
            let agent = store::resolve(&l).unwrap_or_else(|e| die(e));
            let s = Settings { agent, agent_key: l.key, identity: identity(), name };
            eprintln!("pairing with {agent} ...");
            if let Err(e) = pair(&s, &l.code, &STOP) {
                die(e);
            }
            store::save_paired(&dir, &Paired { agent, key: l.key }).unwrap_or_else(|e| die(format!("saving the pairing: {e}")));
            println!("paired with {agent}. Start playing with: meshwav-receiver run");
        }
        "run" => {
            let p = store::load_paired(&dir).unwrap_or_else(|e| die(e)).unwrap_or_else(|| die("not paired yet; see `meshwav-receiver pair`"));
            let s = Settings { agent: p.agent, agent_key: p.key, identity: identity(), name };
            let sink: Box<dyn Sink> = match &output {
                Some(f) => Box::new(RawFile::create(f).unwrap_or_else(|e| die(format!("--output: {e}")))),
                None => Box::new(Pacat::start(latency).unwrap_or_else(|e| die(e))),
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
        "" | "help" | "--help" | "-h" => print!("{USAGE}"),
        other => die(format!("unknown command `{other}`\n\n{USAGE}")),
    }
}
