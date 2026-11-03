// Works out the version to release. Run from the repo root:
//   node scripts/next-version.mjs patch          (or minor, major)
//   node scripts/next-version.mjs patch 0.2.0    (an exact version wins over the bump)
// The last release is the highest vX.Y.Z tag on GitHub. Release tags only appear once a
// release is published, so a run that failed is simply retried with the same version.
// Prints `last=` and `version=` lines, as the Release workflow (.github/workflows/release.yml)
// reads them.
import { execSync } from 'node:child_process';
import { readFileSync } from 'node:fs';

const [bump, exact] = process.argv.slice(2);

function fail(message) {
  console.error((process.env.GITHUB_ACTIONS ? '::error::' : '') + message);
  process.exit(1);
}

// MSI installers only accept numeric versions, so no "-beta" suffixes.
const parse = (text) => /^(\d+)\.(\d+)\.(\d+)$/.exec(text)?.slice(1).map(Number);
const compare = (a, b) => a[0] - b[0] || a[1] - b[1] || a[2] - b[2];

const last = execSync('git ls-remote --tags origin', { encoding: 'utf8' })
  .split('\n')
  .map((line) => parse(line.split('refs/tags/v')[1] ?? ''))
  .filter(Boolean)
  .sort(compare)
  .at(-1) ?? [0, 0, 0];

let next;
if (exact) {
  next = parse(exact);
  if (!next) fail(`The version must look like 1.2.3, got: ${exact}`);
} else {
  const [major, minor, patch] = last;
  next = { major: [major + 1, 0, 0], minor: [major, minor + 1, 0], patch: [major, minor, patch + 1] }[bump];
  if (!next) fail(`Choose patch, minor or major, got: ${bump ?? '(nothing)'}`);
}
if (compare(next, last) <= 0) {
  fail(`${next.join('.')} isn't newer than the last release, ${last.join('.')}.`);
}
// package.json may be ahead of the last release after a failed run; never go below it.
const current = parse(JSON.parse(readFileSync('package.json', 'utf8')).version);
if (current && compare(current, next) > 0) {
  fail(`package.json is already at ${current.join('.')}, newer than ${next.join('.')}. Enter ${current.join('.')} or higher as the version.`);
}

console.log(`last=${last.join('.')}`);
console.log(`version=${next.join('.')}`);
