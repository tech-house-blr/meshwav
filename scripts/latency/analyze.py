#!/usr/bin/env python3
"""Analyze a click_test.sh recording: ch0 = reference clicks, ch1 = microphone.

Prints per-click latency (mic onset minus reference onset) and p50/p95/max.
Usage: analyze.py recording.wav
"""
import sys
import wave

import numpy as np


def load(path):
    w = wave.open(path, "rb")
    rate = w.getframerate()
    ch = w.getnchannels()
    width = w.getsampwidth()
    raw = w.readframes(w.getnframes())
    dt = {2: "<i2", 4: "<i4"}[width]
    x = np.frombuffer(raw, dtype=dt).reshape(-1, ch).astype(np.float64) / (2 ** (8 * width - 1))
    return rate, x


def envelope(x, rate):
    h = np.diff(x, prepend=x[0])  # crude high-pass: ignore rumble and DC
    k = max(1, int(rate * 0.001))
    return np.convolve(np.abs(h), np.ones(k) / k, mode="same")


def ref_onsets(env, rate):
    thr = max(0.05 * env.max(), 1e-4)
    out, last = [], -10**9
    for i in np.flatnonzero(env > thr):
        if i - last > rate * 0.5:
            out.append(i)
        last = i
    return out


def main():
    rate, x = load(sys.argv[1])
    ref, mic = envelope(x[:, 0], rate), envelope(x[:, 1], rate)
    noise = np.median(mic)
    mad = np.median(np.abs(mic - noise))
    thr = noise + 12 * 1.4826 * mad
    lat, missed = [], 0
    for r in ref_onsets(ref, rate):
        lo, hi = r, r + int(rate * 0.4)
        seg = mic[lo:hi]
        hits = np.flatnonzero(seg > max(thr, 0.15 * seg.max()))
        if len(seg) == 0 or seg.max() < 3 * thr or len(hits) == 0:
            missed += 1
            continue
        lat.append((lo + hits[0] - r) * 1000 / rate)
    if not lat:
        print("no clicks detected in the microphone channel")
        return
    a = np.array(lat)
    print("clicks: %d detected, %d missed" % (len(a), missed))
    print("latency ms: p50 %.1f  p95 %.1f  min %.1f  max %.1f  mean %.1f  sd %.1f"
          % (np.percentile(a, 50), np.percentile(a, 95), a.min(), a.max(), a.mean(), a.std()))
    print("per click:", " ".join("%.0f" % v for v in a))


if __name__ == "__main__":
    main()
