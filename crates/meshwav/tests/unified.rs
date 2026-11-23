//! The single `meshwav` program drives both sides: `send` runs the agent, `pair CODE` pairs a receiver,
//! and `devices` talks to the running sender.
use std::io::{BufRead, BufReader};
use std::net::UdpSocket;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Kill(Child);
impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn home(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("mw-unified-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("bin")).unwrap();
    let parec = d.join("bin/parec");
    std::fs::write(&parec, "#!/bin/sh\nwhile true; do head -c 1920 /dev/zero || exit 0; sleep 0.01; done\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&parec, std::fs::Permissions::from_mode(0o755)).unwrap();
    d
}

fn meshwav(home: &PathBuf) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_meshwav"));
    c.env("HOME", home).env_remove("XDG_CONFIG_HOME").env("PATH", format!("{}:{}", home.join("bin").display(), std::env::var("PATH").unwrap()));
    c
}

#[test]
fn send_then_pair_a_receiver_and_list_it_with_the_one_program() {
    let sender = home("sender");
    let receiver = home("receiver");
    let port = UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut child = meshwav(&sender)
        .args(["send", "--bind", &format!("127.0.0.1:{port}"), "--source", "fake.monitor", "--pair"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    for stream in [child.stdout.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>), child.stderr.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>)].into_iter().flatten() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for l in BufReader::new(stream).lines().map_while(Result::ok) {
                let _ = tx.send(l);
            }
        });
    }
    let _agent = Kill(child);

    let end = Instant::now() + Duration::from_secs(10);
    let mut link = None;
    while link.is_none() && Instant::now() < end {
        if let Ok(l) = rx.recv_timeout(Duration::from_millis(200)) {
            if let Some(i) = l.find("meshwav://pair?") {
                link = Some(l[i..].split_whitespace().next().unwrap().to_string());
            }
        }
    }
    let link = link.expect("the sender printed no pairing link");

    let out = meshwav(&receiver).args(["pair", &link, "--name", "unified-rx"]).output().unwrap();
    assert!(out.status.success(), "pair failed: {}", String::from_utf8_lossy(&out.stderr));
    assert!(receiver.join(".config/meshwav/receiver/identity").exists(), "receiver files belong under ~/.config/meshwav/receiver");

    let devices = meshwav(&sender).arg("devices").output().unwrap();
    let text = String::from_utf8_lossy(&devices.stdout).to_string();
    assert!(devices.status.success() && text.contains("unified-rx"), "devices said: {text}");

    let help = meshwav(&receiver).arg("--help").output().unwrap();
    assert!(String::from_utf8_lossy(&help.stdout).contains("meshwav send"));
    let _ = std::fs::remove_dir_all(&sender);
    let _ = std::fs::remove_dir_all(&receiver);
}

/// The receiver asks a real sender for lossless and back to Opus while audio flows.
#[test]
fn a_receiver_can_switch_a_real_sender_between_opus_and_lossless() {
    use meshwav_receiver::store::{parse_pair_link, resolve};
    use meshwav_receiver::{pair, run, Settings, Status};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let sender = home("codec-sender");
    let port = UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut child = meshwav(&sender)
        .args(["send", "--bind", &format!("127.0.0.1:{port}"), "--source", "fake.monitor", "--pair"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let out = child.stdout.take().unwrap();
    let _agent = Kill(child);
    let mut lines = BufReader::new(out).lines().map_while(Result::ok);
    let link = lines.by_ref().find(|l| l.contains("meshwav://pair?")).expect("pairing link"); // keep `lines` alive: a closed pipe would kill the sender
    let l = parse_pair_link(&link).unwrap();
    let s = Settings { agent: resolve(&l).unwrap(), agent_key: l.key, identity: audio_protocol::handshake::Keypair::generate(), name: "codec-rx".into() };
    pair(&s, &l.code, &AtomicBool::new(false)).expect("pairing");

    let (stop, status) = (Arc::new(AtomicBool::new(false)), Arc::new(Status::default()));
    let (s2, st2, sa2) = (s, stop.clone(), status.clone());
    let t = std::thread::spawn(move || run(&s2, Box::new(|_: &[i16]| { std::thread::sleep(Duration::from_millis(4)); Ok(()) }), &st2, &sa2));
    let wait = |want: bool| {
        let end = Instant::now() + Duration::from_secs(15);
        while Instant::now() < end {
            let sn = status.snapshot();
            if sn.lossless == want && sn.frame_ms != 0 && sn.state == "Playing" {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    };
    assert!(wait(false), "starts as Opus: {:?}", status.snapshot());
    status.set_lossless(true);
    assert!(wait(true), "lossless was not granted: {:?}", status.snapshot());
    status.set_lossless(false);
    assert!(wait(false), "did not go back to Opus: {:?}", status.snapshot());
    stop.store(true, Ordering::Relaxed);
    t.join().unwrap().unwrap();
}
