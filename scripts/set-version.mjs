// Sets the app version everywhere it's recorded. Run from the repo root:
//   node scripts/set-version.mjs 0.2.0
// Used by the Release workflow (.github/workflows/release.yml).
import { readFileSync, writeFileSync } from 'node:fs';

const version = process.argv[2];
// MSI installers only accept numeric versions, so no "-beta" suffixes.
if (!/^\d+\.\d+\.\d+$/.test(version ?? '')) {
  console.error(`Version must look like 1.2.3, got: ${version ?? '(nothing)'}`);
  process.exit(1);
}

function edit(path, change) {
  const before = readFileSync(path, 'utf8');
  const after = change(before);
  if (after === before) throw new Error(`${path}: version not found`);
  writeFileSync(path, after);
}

// npm writes these as 2-space JSON with a trailing newline, so a round trip keeps the formatting.
for (const path of ['package.json', 'package-lock.json']) {
  const json = JSON.parse(readFileSync(path, 'utf8'));
  json.version = version;
  if (json.packages?.['']) json.packages[''].version = version;
  writeFileSync(path, JSON.stringify(json, null, 2) + '\n');
}

// Text replacements keep the hand-written formatting of these files.
edit('src-tauri/tauri.conf.json', (s) => s.replace(/("version":\s*")[^"]+"/, `$1${version}"`));
edit('src-tauri/Cargo.toml', (s) => s.replace(/^(version\s*=\s*")[^"]+"/m, `$1${version}"`));
edit('src-tauri/Cargo.lock', (s) =>
  s.replace(/(\[\[package\]\]\r?\nname = "unified-mail"\r?\nversion = ")[^"]+"/, `$1${version}"`));

console.log(`Version set to ${version}`);
