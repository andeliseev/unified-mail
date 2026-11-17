// Makes the app's icon files in src-tauri/icons/ from app-icon.svg, with the Tauri CLI. Run from
// the repo root after changing the icon:
//   node scripts/icons.mjs
// macOS draws app icons on a grid with room around the tile, which app-icon.svg has (the tile is
// 20 of its 24 units). Windows shows icons as they are, so in the taskbar that tile looked smaller
// than other apps' icons. The Windows icon (icon.ico: the taskbar, the window, the .exe and the
// installers) is the same drawing scaled up until the tile fills its square.
import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const WINDOWS_SCALE = 1.2; // 20 units × 1.2 = the whole 24
const OUT = 'src-tauri/icons';
const MAC_AND_PNG = ['32x32.png', '64x64.png', '128x128.png', '128x128@2x.png', 'icon.png', 'icon.icns'];

const work = mkdtempSync(join(tmpdir(), 'unified-mail-icons-'));
try {
  const svg = readFileSync('app-icon.svg', 'utf8');
  // Everything inside the root <svg> element, scaled about the center of the 24-unit square.
  const windows = svg.replace(
    /(<svg[^>]*>)([\s\S]*)(<\/svg>)/,
    (_, open, body, close) => `${open}<g transform="translate(12 12) scale(${WINDOWS_SCALE}) translate(-12 -12)">${body}</g>${close}`,
  );
  writeFileSync(join(work, 'windows.svg'), windows);
  // The CLI's own script, run by this Node: no shell needed for npx on Windows.
  const icon = (input, dir) =>
    execFileSync(process.execPath, ['node_modules/@tauri-apps/cli/tauri.js', 'icon', input, '-o', dir], { stdio: 'ignore' });
  icon('app-icon.svg', join(work, 'all'));
  icon(join(work, 'windows.svg'), join(work, 'windows'));
  // Only the files the app uses; the CLI also makes Android, iOS and Windows Store icons.
  for (const file of MAC_AND_PNG) copyFileSync(join(work, 'all', file), join(OUT, file));
  copyFileSync(join(work, 'windows', 'icon.ico'), join(OUT, 'icon.ico'));
  console.log(`Updated ${[...MAC_AND_PNG, 'icon.ico'].join(', ')} in ${OUT}`);
} finally {
  rmSync(work, { recursive: true, force: true });
}
