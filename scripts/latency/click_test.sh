#!/bin/bash
# Click-loopback latency test. Run on the machine that runs meshwav-agent.
#
# Setup (see docs/measurements.md):
#   - the agent runs with `--no-redirect --source mwtest.monitor`
#   - the phone plays through its own speaker, placed next to this machine's microphone
# The script plays N clicks into the null sink `mwtest` (what the agent captures) and records two
# channels with one ffmpeg process: ch0 = mwtest.monitor (the reference), ch1 = the microphone.
# Then analyze.py compares the click times of the two channels.
set -euo pipefail
MIC=${MIC:-alsa_input.usb-Maono_DGM20_USB_Microphone_20230101-00.analog-stereo}
N=${N:-50}
GAP=${GAP:-1.2}
OUT=${OUT:-/tmp/mw-latency.wav}
CLICKS=/tmp/mw-clicks.wav

python3 - "$N" "$GAP" "$CLICKS" <<'PY'
import random, struct, sys, wave
n, gap, path = int(sys.argv[1]), float(sys.argv[2]), sys.argv[3]
rate = 48000
random.seed(1)
burst = [random.uniform(-1, 1) * (1 - i / 240) for i in range(240)]  # 5 ms noise burst, fast decay
total = int(rate * (gap * n + 1))
buf = [0.0] * total
for k in range(n):
    s = int(rate * (0.5 + k * gap))
    for i, v in enumerate(burst):
        buf[s + i] = v * 0.7
w = wave.open(path, "wb")
w.setnchannels(2); w.setsampwidth(2); w.setframerate(rate)
w.writeframes(b"".join(struct.pack("<hh", int(v * 32767), int(v * 32767)) for v in buf))
w.close()
PY

if ! pactl list short sinks | grep -q "[[:space:]]mwtest[[:space:]]"; then
  MOD=$(pactl load-module module-null-sink sink_name=mwtest sink_properties=device.description=mwtest)
  trap 'pactl unload-module "$MOD" 2>/dev/null || true' EXIT
fi

DUR=$(python3 -c "print(int($N*$GAP+4))")
ffmpeg -y -loglevel error -f pulse -i mwtest.monitor -f pulse -i "$MIC" \
  -filter_complex "[0:a]pan=mono|c0=c0[a];[1:a]pan=mono|c0=c0[b];[a][b]amerge=inputs=2[o]" \
  -map "[o]" -ar 48000 -t "$DUR" "$OUT" &
FF=$!
sleep 1.5
paplay --device=mwtest "$CLICKS"
wait $FF
echo "recorded $OUT"
