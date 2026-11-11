#!/bin/bash
# Stands in for `codesign` while the Release workflow builds the Mac app: its "Set up Mac signing"
# step puts this first in PATH under that name (docs/development.md#mac-signing).
#
# Tauri signs the app's executable, the app and the .dmg with
#   codesign --force -s "Unified Mail" [--options runtime] <path>
# and this signs them with rcodesign and the certificate's .p12 instead. codesign won't sign with a
# self-signed certificate that the Mac it runs on doesn't trust, and GitHub's Mac runners can't be
# made to trust one. Any other call goes to the real codesign.
#
# Reads RCODESIGN (the rcodesign program), SIGNING_IDENTITY ("Unified Mail"), SIGNING_P12 and
# SIGNING_P12_PASSWORD_FILE from the environment.
set -euo pipefail

identity="" runtime=false paths=()
for ((i = 1; i <= $#; i++)); do
  case "${!i}" in
    -f | --force) ;;
    -s | --sign) i=$((i + 1)); identity="${!i:-}" ;;
    --options) i=$((i + 1)); [[ ",${!i:-}," == *,runtime,* ]] && runtime=true ;;
    -*) exec /usr/bin/codesign "$@" ;;
    *) paths+=("${!i}") ;;
  esac
done
if [ "$identity" != "$SIGNING_IDENTITY" ] || [ "${#paths[@]}" -ne 1 ]; then
  exec /usr/bin/codesign "$@"
fi

args=(sign --p12-file "$SIGNING_P12" --p12-password-file "$SIGNING_P12_PASSWORD_FILE" --timestamp-url none)
if $runtime; then
  args+=(--code-signature-flags runtime)
fi
echo "Signing ${paths[0]} with rcodesign"
exec "$RCODESIGN" "${args[@]}" "${paths[0]}"
