#!/bin/bash
# Makes the self-signed certificate the Release workflow signs the Mac app with, and saves it as
# the APPLE_CERTIFICATE and APPLE_CERTIFICATE_PASSWORD repository secrets. One time, on a Mac,
# from this repository, with the GitHub CLI signed in (docs/development.md#mac-signing):
#
#   scripts/mac-signing-certificate.sh [folder for the backup copy]
#
# Every release signed with the same certificate is the same app to the keychain. A new one makes
# every Mac ask for the keychain password once more, so keep the backup.
set -euo pipefail

name="Unified Mail"  # bundle.macOS.signingIdentity in src-tauri/tauri.release.conf.json
days=7300            # 20 years
backup="${1:-$HOME/Unified Mail signing}"
# macOS's own openssl (LibreSSL) writes a .p12 that `security import` reads on every macOS version.
ssl=/usr/bin/openssl

if ! gh auth status >/dev/null 2>&1; then
  echo "Sign in to the GitHub CLI first: gh auth login" >&2
  exit 1
fi
if gh secret list | grep -q '^APPLE_CERTIFICATE[[:space:]]'; then
  echo "The repository already has an APPLE_CERTIFICATE secret. A new certificate would make every" >&2
  echo "Mac ask for the keychain password once more. To replace it anyway, delete the secret first:" >&2
  echo "  gh secret delete APPLE_CERTIFICATE" >&2
  exit 1
fi
if [ -e "$backup" ]; then
  echo "$backup already exists. Pass another folder for the backup copy." >&2
  exit 1
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cat > "$work/cert.cnf" <<EOF
[req]
distinguished_name = dn
prompt = no
x509_extensions = ext
[dn]
CN = $name
[ext]
basicConstraints = critical, CA:FALSE
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
subjectKeyIdentifier = hash
EOF
"$ssl" req -x509 -newkey rsa:2048 -nodes -days "$days" -config "$work/cert.cnf" \
  -keyout "$work/key.pem" -out "$work/cert.pem" 2>/dev/null
password=$("$ssl" rand -base64 24)

mkdir -p "$backup"
chmod 700 "$backup"
p12="$backup/$name signing.p12"
PASSWORD="$password" "$ssl" pkcs12 -export -inkey "$work/key.pem" -in "$work/cert.pem" -name "$name" \
  -out "$p12" -passout env:PASSWORD
printf '%s\n' "$password" > "$backup/password.txt"
cp "$work/cert.pem" "$backup/$name signing.pem"
chmod 600 "$p12" "$backup/password.txt"

base64 -i "$p12" | gh secret set APPLE_CERTIFICATE
printf '%s' "$password" | gh secret set APPLE_CERTIFICATE_PASSWORD

fingerprint=$("$ssl" x509 -in "$work/cert.pem" -noout -fingerprint -sha1 | sed 's/.*=//; s/://g')
echo
echo "Done: the repository has the APPLE_CERTIFICATE and APPLE_CERTIFICATE_PASSWORD secrets."
echo "Releases from now on are signed with \"$name\" (SHA-1 $fingerprint)."
echo
echo "Keep a copy of $backup (the .p12 and password.txt) somewhere safe, such as a password"
echo "manager, then delete the folder. GitHub can't show the secrets again, and a new certificate"
echo "would make every Mac ask for the keychain password once more."
