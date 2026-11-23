//! meshwav: one program for both directions. `send` is the former agent, `receive` the former receiver.

const USAGE: &str = "meshwav: private low-latency audio over your tailnet

usage:
  meshwav send [options]        send this computer's audio (runs until stopped)
  meshwav receive <command>     play audio sent by another device
      pair CODE | run | outputs | forget
  meshwav pair                  open the pairing window on the running sender
  meshwav pair CODE             pair this computer as a receiver with the code a sender shows
  meshwav devices               list receivers paired with the running sender
  meshwav use <device|auto>     choose which receiver plays
  meshwav rename <device> <new name>

`meshwav send --help` and `meshwav receive help` list each side's options.
";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rest = |n: usize| args.iter().skip(n).cloned().collect::<Vec<_>>();
    match args.get(1).map(String::as_str) {
        Some("send") => meshwav_agent::run([vec!["meshwav send".to_string()], rest(2)].concat()),
        Some("receive") => meshwav_receiver::cli::run_cli("meshwav receive", rest(2)),
        // `pair` with no argument talks to the running sender; with a code it pairs this receiver.
        Some("pair") if args.len() > 2 && !args[2].starts_with("--") => meshwav_receiver::cli::run_cli("meshwav", rest(1)),
        Some("pair" | "devices" | "use" | "rename") => meshwav_agent::run([vec!["meshwav".to_string()], rest(1)].concat()),
        Some("outputs") => meshwav_receiver::cli::run_cli("meshwav", rest(1)),
        None | Some("help" | "--help" | "-h") => print!("{USAGE}"),
        Some(other) => {
            eprintln!("meshwav: unknown command `{other}`\n\n{USAGE}");
            std::process::exit(2);
        }
    }
}
