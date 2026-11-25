#!/bin/sh
# Sign an Android APK with the Meshwav release key. Holds no secrets: the key store is a file you keep
# (default: the Tech House vault) and apksigner asks for its password on the terminal.
#
#   scripts/sign-apk.sh app-release-unsigned.apk meshwav-0.9.0.apk
#   scripts/sign-apk.sh --make-lineage         # once: let the new key take over from the old debug key
#
# Environment (all optional):
#   MESHWAV_KEYSTORE   release key store (default $HOME/Tech_House_Sync/Assets/meshwav-signing/meshwav-release.p12)
#   MESHWAV_KEY_ALIAS  alias inside it (default meshwav)
#   MESHWAV_LINEAGE    key-rotation file (default next to the key store, lineage.bin). If it exists the APK is
#                      signed so phones that have an APK signed with the old key accept it as an update.
#   DEBUG_KEYSTORE     the old key, only for --make-lineage (default $HOME/.android/debug.keystore)
#   ANDROID_HOME       Android SDK with build-tools (default $HOME/Projects/meshwav-toolchain/android-sdk)
# minSdk is 28, so the old v1 JAR scheme is off. With a key rotation only the v3 scheme is used (v1 and v2 cannot
# carry it), and the rotation must apply from Android 9 (SDK 28), our minimum, not just from Android 13.
set -eu

KS=${MESHWAV_KEYSTORE:-$HOME/Tech_House_Sync/Assets/meshwav-signing/meshwav-release.p12}
ALIAS=${MESHWAV_KEY_ALIAS:-meshwav}
LINEAGE=${MESHWAV_LINEAGE:-$(dirname "$KS")/lineage.bin}
SDK=${ANDROID_HOME:-$HOME/Projects/meshwav-toolchain/android-sdk}
BT=$(ls -d "$SDK"/build-tools/*/ 2>/dev/null | sort -V | tail -1)
BT=${BT%/}
[ -x "$BT/apksigner" ] || { echo "apksigner not found under $SDK/build-tools (set ANDROID_HOME)" >&2; exit 2; }
[ -f "$KS" ] || { echo "release key store not found: $KS (set MESHWAV_KEYSTORE)" >&2; exit 2; }

if [ "${1:-}" = "--make-lineage" ]; then
  OLD=${DEBUG_KEYSTORE:-$HOME/.android/debug.keystore}
  [ -f "$OLD" ] || { echo "old key store not found: $OLD (set DEBUG_KEYSTORE)" >&2; exit 2; }
  [ ! -e "$LINEAGE" ] || { echo "$LINEAGE already exists; not overwriting it" >&2; exit 2; }
  echo "Creating $LINEAGE (old debug key -> release key). Enter the release key store password when asked."
  "$BT/apksigner" rotate --out "$LINEAGE" \
    --old-signer --ks "$OLD" --ks-pass pass:android \
    --new-signer --ks "$KS" --ks-key-alias "$ALIAS"
  chmod 600 "$LINEAGE"
  echo "Done. Keep the lineage file with the key store; every future APK must be signed with it."
  exit 0
fi

[ $# -eq 2 ] || { echo "usage: $0 IN.apk OUT.apk | $0 --make-lineage" >&2; exit 2; }
IN=$1
OUT=$2
[ -f "$IN" ] || { echo "no such file: $IN" >&2; exit 2; }
[ "$IN" != "$OUT" ] || { echo "OUT must differ from IN" >&2; exit 2; }

TMP=$(mktemp "${TMPDIR:-/tmp}/meshwav-aligned.XXXXXX")
trap 'rm -f "$TMP"' EXIT
"$BT/zipalign" -f -p 4 "$IN" "$TMP"

if [ -f "$LINEAGE" ]; then
  echo "Signing with the release key, with the rotation lineage $LINEAGE."
  "$BT/apksigner" sign --ks "$KS" --ks-key-alias "$ALIAS" --v1-signing-enabled false --v2-signing-enabled false --rotation-min-sdk-version 28 --lineage "$LINEAGE" --out "$OUT" "$TMP"
else
  echo "Signing with the release key (no lineage: phones with an APK signed by another key cannot update in place)."
  "$BT/apksigner" sign --ks "$KS" --ks-key-alias "$ALIAS" --v1-signing-enabled false --out "$OUT" "$TMP"
fi

"$BT/apksigner" verify --verbose --print-certs "$OUT" | grep -E "^Verifies|Verified using|certificate SHA-256 digest|Number of signers"
echo "Signed APK: $OUT"
