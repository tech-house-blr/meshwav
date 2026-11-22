use audio_codec::flac::*;
use audio_codec::opus::*;
use audio_codec::pcm::*;

fn music_like(n: usize, rate: u32, depth: u8) -> Vec<i32> {
    // two tones + harmonic + low-level noise, stereo with correlated channels
    let amp = ((1i64 << (depth - 1)) as f64 * 0.5) as f64;
    let mut x = 12345u64;
    let mut out = Vec::with_capacity(n * 2);
    for i in 0..n {
        let t = i as f64 / rate as f64;
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let noise = ((x >> 40) as f64 / (1u64 << 24) as f64 - 0.5) * amp * 0.002;
        let l = amp * 0.6 * (2.0 * std::f64::consts::PI * 440.0 * t).sin() + amp * 0.2 * (2.0 * std::f64::consts::PI * 1320.0 * t).sin() + noise;
        let r = amp * 0.6 * (2.0 * std::f64::consts::PI * 440.0 * t + 0.3).sin() + amp * 0.2 * (2.0 * std::f64::consts::PI * 2000.0 * t).sin() + noise;
        out.push(l as i32);
        out.push(r as i32);
    }
    out
}

#[test]
fn pcm_roundtrip_all_depths_and_extremes() {
    let s16 = vec![0, 1, -1, i16::MAX as i32, i16::MIN as i32];
    let mut b = Vec::new();
    pack_int(&s16, PcmDepth::S16, &mut b).unwrap();
    assert_eq!(unpack_int(&b, PcmDepth::S16).unwrap(), s16);
    let s24 = vec![0, 1, -1, (1 << 23) - 1, -(1 << 23), 123456, -654321];
    let mut b = Vec::new();
    pack_int(&s24, PcmDepth::S24, &mut b).unwrap();
    assert_eq!(b.len(), s24.len() * 3);
    assert_eq!(unpack_int(&b, PcmDepth::S24).unwrap(), s24);
    let f = vec![0.0f32, -0.5, 0.999, f32::MIN_POSITIVE];
    let mut b = Vec::new();
    pack_f32(&f, &mut b);
    assert_eq!(unpack_f32(&b).unwrap(), f);
}

#[test]
fn pcm_rejects_out_of_range_and_bad_length() {
    let mut b = Vec::new();
    assert!(pack_int(&[40000], PcmDepth::S16, &mut b).is_err());
    assert!(pack_int(&[1 << 23], PcmDepth::S24, &mut b).is_err());
    assert!(unpack_int(&[1, 2, 3, 4], PcmDepth::S24).is_err());
    assert!(unpack_f32(&[1, 2, 3]).is_err());
}

#[test]
fn flac_is_bit_exact_for_all_rates_and_depths() {
    for &(rate, depth, block) in &[(44100u32, 16u8, 441u16), (48000, 16, 480), (48000, 24, 480), (96000, 24, 480), (192000, 24, 480), (192000, 24, 240)] {
        let p = FlacParams { rate, channels: 2, depth, block };
        let enc = FlacEncoder::new(p).unwrap();
        let dec = FlacDecoder::new(p).unwrap();
        let pcm = music_like(block as usize * 20, rate, depth);
        let mut total_in = 0usize;
        let mut total_out = 0usize;
        for chunk in pcm.chunks(block as usize * 2) {
            let frame = enc.encode(chunk).unwrap();
            assert_eq!(dec.decode(&frame).unwrap(), chunk, "rate {rate} depth {depth}");
            total_in += chunk.len() * (depth as usize / 8);
            total_out += frame.len();
        }
        eprintln!("flac {rate}/{depth} block {block}: {:.0}% of raw PCM", 100.0 * total_out as f64 / total_in as f64);
    }
}

#[test]
fn flac_survives_full_scale_extremes_and_silence() {
    let p = FlacParams { rate: 96000, channels: 2, depth: 24, block: 480 };
    let enc = FlacEncoder::new(p).unwrap();
    let dec = FlacDecoder::new(p).unwrap();
    let silence = vec![0i32; 960];
    assert_eq!(dec.decode(&enc.encode(&silence).unwrap()).unwrap(), silence);
    let square: Vec<i32> = (0..960).map(|i| if (i / 2) % 20 < 10 { (1 << 23) - 1 } else { -(1 << 23) }).collect();
    assert_eq!(dec.decode(&enc.encode(&square).unwrap()).unwrap(), square);
}

#[test]
fn flac_frames_are_independent_so_loss_does_not_propagate() {
    let p = FlacParams { rate: 48000, channels: 2, depth: 16, block: 480 };
    let enc = FlacEncoder::new(p).unwrap();
    let dec = FlacDecoder::new(p).unwrap();
    let pcm = music_like(480 * 3, 48000, 16);
    let frames: Vec<_> = pcm.chunks(960).map(|c| enc.encode(c).unwrap()).collect();
    // decode frame 2 without ever seeing 0 and 1, with a fresh decoder
    let fresh = FlacDecoder::new(p).unwrap();
    assert_eq!(fresh.decode(&frames[2]).unwrap(), &pcm[1920..]);
    assert!(dec.decode(&frames[0]).is_ok());
}

#[test]
fn flac_rejects_corrupt_and_wrong_size_input() {
    let p = FlacParams { rate: 48000, channels: 2, depth: 16, block: 480 };
    let enc = FlacEncoder::new(p).unwrap();
    let dec = FlacDecoder::new(p).unwrap();
    assert!(enc.encode(&[0; 10]).is_err());
    let mut f = enc.encode(&music_like(480, 48000, 16)).unwrap();
    let mid = f.len() / 2;
    f[mid] ^= 0xff;
    assert!(dec.decode(&f).is_err(), "corruption must be detected by the frame CRC");
    assert!(dec.decode(&[]).is_err());
    assert!(dec.decode(&[0xff, 0xf8, 0, 0]).is_err());
}

#[test]
fn flac_frame_fits_mtu_for_stereo_48k_16_but_high_res_may_fragment() {
    use audio_protocol_limit::MAX_PAYLOAD;
    let p = FlacParams { rate: 48000, channels: 2, depth: 16, block: 480 };
    let f = FlacEncoder::new(p).unwrap().encode(&music_like(480, 48000, 16)).unwrap();
    assert!(f.len() <= MAX_PAYLOAD, "{} bytes", f.len());
}
mod audio_protocol_limit { pub const MAX_PAYLOAD: usize = 1200 - 20 - 16; }

fn snr_db(a: &[i16], b: &[i16]) -> f64 {
    let sig: f64 = a.iter().map(|&x| (x as f64).powi(2)).sum();
    let err: f64 = a.iter().zip(b).map(|(&x, &y)| ((x - y) as f64).powi(2)).sum();
    10.0 * (sig / err.max(1e-9)).log10()
}

#[test]
fn opus_roundtrip_all_frame_sizes_and_bitrates() {
    for &us in &FRAME_US {
        for &kbps in &[64u32, 160, 256] {
            let mut enc = OpusEncoder::new(us, kbps, false).unwrap();
            let mut dec = OpusDecoder::new(us).unwrap();
            let n = enc.frame_samples();
            let pcm: Vec<i16> = music_like(48000, 48000, 16).iter().map(|&x| x as i16).collect();
            let mut decoded = Vec::new();
            let mut bytes = 0usize;
            for chunk in pcm.chunks_exact(n * 2) {
                let pkt = enc.encode(chunk).unwrap().to_vec();
                assert!(pkt.len() <= 1164, "opus packet must fit one datagram");
                bytes += pkt.len();
                let out = dec.decode(Some(&pkt)).unwrap();
                assert_eq!(out.len(), n * 2);
                decoded.extend(out);
            }
            // Opus has algorithmic delay; compare skipping the first 10 frames after best lag search.
            let lag_best = (0..1500).step_by(2).map(|l| (l, snr_db(&pcm[20000..30000], &decoded[20000 + l..30000 + l]))).max_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
            let (_, snr) = lag_best.unwrap();
            let actual_kbps = bytes as f64 * 8.0 / (pcm.len() as f64 / 2.0 / 48000.0) / 1000.0;
            eprintln!("opus {us}us {kbps}kbps: snr {snr:.1} dB, actual {actual_kbps:.0} kbps");
            assert!(snr > 10.0, "{us}us {kbps}kbps snr {snr}");
        }
    }
}

#[test]
fn opus_plc_produces_full_frames_and_recovers() {
    let mut enc = OpusEncoder::new(10_000, 160, false).unwrap();
    let mut dec = OpusDecoder::new(10_000).unwrap();
    let pcm: Vec<i16> = music_like(480 * 30, 48000, 16).iter().map(|&x| x as i16).collect();
    let mut plc_energy = 0f64;
    for (i, chunk) in pcm.chunks(960).enumerate() {
        let pkt = enc.encode(chunk).unwrap().to_vec();
        let out = if (10..13).contains(&i) { dec.decode(None).unwrap() } else { dec.decode(Some(&pkt)).unwrap() };
        assert_eq!(out.len(), 960);
        if i == 10 { plc_energy = out.iter().map(|&x| (x as f64).powi(2)).sum(); }
    }
    assert!(plc_energy > 0.0, "PLC should extrapolate audio, not emit digital silence");
}

#[test]
fn opus_parameter_validation() {
    assert!(OpusEncoder::new(7_000, 160, false).is_err());
    assert!(OpusEncoder::new(10_000, 32, false).is_err());
    assert!(OpusEncoder::new(10_000, 300, false).is_err());
    let mut e = OpusEncoder::new(10_000, 160, false).unwrap();
    assert!(e.encode(&[0; 10]).is_err());
    assert!(e.set_bitrate_kbps(500).is_err());
    assert!(e.set_bitrate_kbps(96).is_ok());
    let mut d = OpusDecoder::new(10_000).unwrap();
    assert!(d.decode(Some(&[])).is_err() || d.decode(Some(&[])).is_ok()); // must not panic
    let _ = d.decode(Some(&[0xff; 50]));
}
