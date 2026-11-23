//! Host check: send a 440 Hz tone as the Android app would.  tone <ip> <port> <code> <seconds>
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (link, src) = android_spike::start(&a[1], a[2].parse().unwrap(), &a[3]).expect("start");
    println!("{link}");
    src.set_lossless(a.get(5).is_some_and(|x| x == "pcm"));
    let secs: u64 = a[4].parse().unwrap();
    let t0 = std::time::Instant::now();
    let mut n = 0u64;
    while t0.elapsed().as_secs() < secs {
        // push 10 ms every 10 ms, like a capture loop
        let mut f = Vec::with_capacity(960);
        for _ in 0..480 {
            let v = ((n as f32 * 440.0 * 2.0 * std::f32::consts::PI / 48000.0).sin() * 8000.0) as i16;
            f.push(v);
            f.push(v);
            n += 1;
        }
        src.push(&f);
        std::thread::sleep(std::time::Duration::from_millis(10));
        if n % 48000 == 0 {
            println!("{} sent={} dropped={}", src.status.lock().unwrap(), src.sent.load(std::sync::atomic::Ordering::Relaxed), src.dropped.load(std::sync::atomic::Ordering::Relaxed));
        }
    }
    src.stop();
}
