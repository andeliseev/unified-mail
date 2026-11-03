// Moves the Unreleased notes in CHANGELOG.md under a new version and prints them, for the
// release notes. Run from the repo root:
//   node scripts/changelog.mjs 0.2.0
// Used by the Release workflow (.github/workflows/release.yml). Run again for a version that's
// already there (a retried release), it changes nothing and prints that version's notes.
import { readFileSync, writeFileSync } from 'node:fs';

const version = process.argv[2];
if (!/^\d+\.\d+\.\d+$/.test(version ?? '')) {
  console.error(`Version must look like 1.2.3, got: ${version ?? '(nothing)'}`);
  process.exit(1);
}

const path = 'CHANGELOG.md';
const text = readFileSync(path, 'utf8');
const eol = text.includes('\r\n') ? '\r\n' : '\n';
const lines = text.split(eol);

// A section's notes are the lines between its "## " heading and the next one.
const headings = lines.flatMap((line, i) => (line.startsWith('## ') ? [i] : []));
const notesOf = (heading) => {
  const next = headings.find((i) => i > heading) ?? lines.length;
  return lines.slice(heading + 1, next).join('\n').trim();
};

const existing = lines.findIndex((line) => line.startsWith(`## ${version} `));
if (existing >= 0) {
  console.log(notesOf(existing));
  process.exit(0);
}

const unreleased = lines.indexOf('## Unreleased');
if (unreleased < 0) {
  console.error('CHANGELOG.md has no "## Unreleased" section.');
  process.exit(1);
}
const notes = notesOf(unreleased);
const date = new Date().toISOString().slice(0, 10);
// The notes move under the version, and a new, empty Unreleased section starts above it.
lines.splice(unreleased, 1, '## Unreleased', '', `## ${version} (${date})`);
writeFileSync(path, lines.join(eol));
console.log(notes);
