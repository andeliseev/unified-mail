'use strict';

const { invoke, Channel } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { getVersion } = window.__TAURI__.app;

const IS_MAC = navigator.userAgent.includes('Macintosh');

const PALETTE = ['#2f6df6', '#d93025', '#188038', '#e37400', '#9334e6', '#00897b', '#c2185b', '#5f6368'];
// Push (IMAP IDLE) triggers syncs instantly; polling is only a safety net.
const AUTO_REFRESH_MS = 5 * 60 * 1000;
// Gmail often sends several push lines per change; sync once for the burst.
const PUSH_DEBOUNCE_MS = 300;
const REFRESH_ON_FOCUS_AFTER_MS = 30 * 1000;
// How long Undo stays available after an action.
const UNDO_MS = 8000;
// More new messages at once than this get one summary notification instead.
const MAX_NOTIFICATIONS = 3;
// How often to ask GitHub for a newer release while the app runs; opening Settings always asks too.
const UPDATE_CHECK_MS = 6 * 60 * 60 * 1000;
// Emails wider than the reader are scaled down to fit, but not below this; past it they scroll.
const MIN_EMAIL_ZOOM = 0.6;
// Search runs once typing has paused this long (or at once on Enter).
const SEARCH_DELAY_MS = 500;
// Dragging the message list's edge (initListResize) sets its width within these. The reader
// keeps READER_MIN, as in the CSS grid, which narrows the list in narrow windows.
const LIST_MIN = 250;
const LIST_MAX = 600;
const READER_MIN = 400;
// HTML emails bring their own margins, so the frame adds little; plain text gets more room.
const FRAME_CSS = `
  html { background: #fff; color: #202124; }
  body { margin: 0; padding: 10px 8px 16px; font: 14px/1.5 -apple-system, "Segoe UI", system-ui, sans-serif; word-wrap: break-word; }
  img { max-width: 100%; height: auto; }
  pre.plain { white-space: pre-wrap; font: inherit; margin: 0; padding: 8px 16px 12px; }
  a { color: #1a5fd6; }
  /* Some emails set overflow: hidden on html/body, which would make them unscrollable.
     :root outranks the emails' html/body selectors, so this wins even against !important. */
  :root { overflow: auto !important; }
  :root > body { overflow: visible !important; }
`;
// The color darkenEmail turns white into: a darkened email's background.
const EMAIL_DARK_BACKGROUND = '#1f1f1f';
// Added in dark mode: the defaults behind an email that doesn't set its own colors, with the
// background darkened white areas blend into. A dark color scheme makes the scroll bar's thumb
// light (see showBody).
const FRAME_DARK_CSS = `
  :root { color-scheme: dark !important; }
  html { background: ${EMAIL_DARK_BACKGROUND}; color: #e3e3e3; }
  a { color: #8ab4f8; }
`;

// Sidebar folders; `icon` is an SVG path.
const FOLDERS = {
  inbox: { name: 'Inbox', empty: 'Inbox zero. Nothing here.', icon: 'M3 13h5l2 3h4l2-3h5M5 5h14l2 8v6H3v-6z' },
  all: { name: 'All Mail', empty: 'No mail here.', icon: 'M4 6h16v12H4zM4 7l8 6 8-6' },
  trash: { name: 'Trash', empty: 'Trash is empty.', icon: 'M3 6h18M8 6V4h8v2M6 6l1 14h10l1-14' },
};
const ids = (m) => ({ account: m.account, id: m.id });

/**
 * Whether the message is in its Inbox. Both lists hold each account's newest messages,
 * so a recent All Mail message that's in the Inbox is always in the Inbox list too.
 * Older All Mail pages report their Inbox messages separately (`inboxExtra`).
 */
const inInbox = (m) => state.inboxExtra.has(keyOf(m)) || state.lists.inbox.some((x) => keyOf(x) === keyOf(m));

/**
 * Actions offered on a message. `inboxKeys`: the keys of everything in the Inbox, when the
 * caller already has them (renderList asks for every row, and `inInbox` searches a list).
 * Search results come from All Mail, so they get All Mail's actions.
 */
function actionsFor(m, inboxKeys = null) {
  switch (listName()) {
    case 'inbox': return ['archive', 'trash'];
    case 'all': case 'search': {
      const inbox = inboxKeys ? inboxKeys.has(keyOf(m)) : inInbox(m);
      return inbox ? ['archive', 'trash'] : ['unarchive', 'trash'];
    }
    default: return ['restore'];
  }
}

/**
 * For each action: its button label, the folder lists the message leaves and joins (applied
 * right away, before Gmail answers), the Gmail call, and how to undo it given that call's result.
 */
const ACTIONS = {
  archive: {
    label: 'Archive',
    done: 'Archived',
    verb: 'archive',
    leave: ['inbox'],
    join: [],
    run: (m) => invoke('archive_message', ids(m)),
    undo: (m) => ({
      leave: [],
      join: ['inbox'],
      run: () => invoke('restore_message', { ...ids(m), fromTrash: false, toInbox: true }),
    }),
  },
  trash: {
    label: 'Delete',
    done: 'Moved to Trash',
    verb: 'delete',
    leave: ['inbox', 'all'],
    join: ['trash'],
    run: (m) => invoke('trash_message', ids(m)),
    undo: (m, { wasInInbox }) => ({
      leave: ['trash'],
      join: wasInInbox ? ['inbox', 'all'] : ['all'],
      run: () => invoke('restore_message', { ...ids(m), fromTrash: true, toInbox: wasInInbox }),
    }),
  },
  // "Move to Inbox" on an archived message in All Mail.
  unarchive: {
    label: 'Move to Inbox',
    done: 'Moved to Inbox',
    verb: 'move',
    leave: [],
    join: ['inbox'],
    run: (m) => invoke('restore_message', { ...ids(m), fromTrash: false, toInbox: true }),
    undo: (m) => ({
      leave: ['inbox'],
      join: [],
      run: () => invoke('archive_message', ids(m)),
    }),
  },
  // "Move to Inbox" in Trash.
  restore: {
    label: 'Move to Inbox',
    done: 'Moved to Inbox',
    verb: 'move',
    leave: ['trash'],
    join: ['inbox', 'all'],
    run: (m) => invoke('restore_message', { ...ids(m), fromTrash: true, toInbox: true }),
    undo: (m) => ({
      leave: ['inbox', 'all'],
      join: ['trash'],
      run: () => invoke('trash_message', ids(m)),
    }),
  },
};

const state = {
  builtInClient: false,  // OAuth client compiled into the app; Settings hides that section
  configured: false,
  theme: 'system',       // Settings → Appearance: system, light or dark
  full: null,            // the open message's body, to show it again with other colors
  originalColors: false, // dark mode: show the open email in its original light colors
  accounts: [],
  expired: new Set(),    // accounts whose sign-in Google no longer accepts: they offer Reconnect
  folder: 'inbox',       // folder chosen in the sidebar
  // Each folder's messages, all accounts, newest first; `search` holds the search's results.
  lists: { inbox: [], all: [], trash: [], search: [] },
  loaded: { inbox: false, all: false, trash: false, search: false },  // list filled from cache or a sync
  synced: { inbox: false, all: false, trash: false },  // a sync finished this session
  // The search, while its field is open: `query` (what the results are for, '' before the first
  // search), `seq` (counts searches, so a late answer is dropped), `done` (the results are in),
  // `timer` (the search waiting for typing to pause), `scroll` (where the folder's list was).
  search: null,
  searchLeft: new Set(), // keys of results an action took out of the search; Undo puts them back
  filter: null,          // account email, or null for all accounts
  selected: null,        // key of the highlighted row
  current: null,         // message shown in the reader
  pending: new Map(),    // key → move Gmail hasn't confirmed yet; re-applied to sync results
  pendingRead: new Map(), // key → read/unread change Gmail hasn't confirmed yet; same idea
  undo: null,            // what the toast's Undo button does, while it shows
  // Per folder (and the search) and account: `frontier`, the oldest date loaded so far without
  // gaps, and `more`, whether older mail exists. Scrolling pushes the frontier back.
  paging: { inbox: {}, all: {}, trash: {}, search: {} },
  inboxExtra: new Map(), // key → date: Inbox messages found on older All Mail pages and in search results
  newestSeen: {},        // account → date of its newest Inbox message so far; newer unread mail notifies
  badge: null,           // unread count shown on the app icon
  update: null,          // a newer version on GitHub, offered in Settings
  loadingOlder: false,
  olderRetryAt: 0,       // after a failed load, don't retry before this time
  syncing: false,
  syncQueued: false,
  lastSync: 0,
  openSeq: 0,
  authSeq: 0,
};

const $ = (sel) => document.querySelector(sel);
const keyOf = (m) => `${m.account}/${m.id}`;
const colorOf = (account) => PALETTE[Math.max(0, state.accounts.indexOf(account)) % PALETTE.length];
const byDate = (a, b) => b.date - a.date;

/** The list on screen: the search's results once a search has run, else the folder's. */
const listName = () => (state.search?.query ? 'search' : state.folder);

/** Accounts on screen that have older mail than what's loaded in `folder`. */
const accountsWithMore = (folder) =>
  (state.filter ? [state.filter] : state.accounts).filter((a) => state.paging[folder][a]?.more);

/**
 * The messages on screen. The list stops at the newest frontier among accounts
 * with more mail to load: below it that account's messages are missing, so the merged
 * list would have gaps. Scrolling loads that account next, and the cut-off moves down.
 */
const visible = () => {
  const folder = listName();
  const cutoff = Math.max(-Infinity, ...accountsWithMore(folder).map((a) => state.paging[folder][a].frontier));
  return state.lists[folder].filter((m) => (!state.filter || m.account === state.filter) && m.date >= cutoff);
};
const byKey = (key) => state.lists[listName()].find((m) => keyOf(m) === key);
/**
 * Whether `move` takes a message out of `list`. Search results come from All Mail, so what
 * leaves All Mail leaves them too.
 */
const leaves = (move, list) => move.leave.includes(list) || (list === 'search' && move.leave.includes('all'));
/** The open message as the folder list has it now; syncs replace the list's objects. */
const openMessage = () => state.current && (byKey(keyOf(state.current)) || state.current);

// ---------- Startup ----------

async function init() {
  initTitleBar();
  initListResize();
  bindEvents();
  const setup = await invoke('get_setup');
  state.builtInClient = setup.builtInClient;
  state.configured = setup.builtInClient || Boolean(setup.clientId);
  state.accounts = setup.accounts;
  state.theme = setup.theme;
  setListWidth(setup.listWidth);
  renderTheme();
  $('#client-section').hidden = setup.builtInClient;
  $('#client-id').value = setup.clientId || '';
  getVersion().then((version) => { $('#app-version').textContent = version; });
  setList('inbox', await invoke('cached_folder', { folder: 'inbox' }));
  render();

  if (!state.configured) openSettings();
  if (state.accounts.length) refresh();

  let pushTimer;
  await listen('mail-changed', () => {
    clearTimeout(pushTimer);
    pushTimer = setTimeout(refresh, PUSH_DEBOUNCE_MS);
  });
  await listen('open-message', (e) => openFromNotification(e.payload));
  // macOS: right-clicks and keys in the email, which WebKit doesn't hand to this page (macos.rs).
  await listen('email-context-menu', (e) => onEmailContextMenu(...e.payload));
  await listen('email-key', (e) => onEmailKey(...e.payload));
  setInterval(() => { if (!document.hidden) refresh(); }, AUTO_REFRESH_MS);
  window.addEventListener('focus', () => {
    if (Date.now() - state.lastSync > REFRESH_ON_FOCUS_AFTER_MS) refresh();
  });
  // Not right at launch, so the first sync goes first.
  setTimeout(checkForUpdate, 10 * 1000);
  setInterval(checkForUpdate, UPDATE_CHECK_MS);
}

/**
 * The window has no grey system title bar (main.rs), so the app's name appears once, at the top
 * of the sidebar. The sidebar's brand, the list header and a strip above the reader drag the
 * window, and a double-click on them maximizes it (zooms on macOS). On macOS the system's
 * window buttons sit over the sidebar, above the brand. On Windows the app draws Minimize,
 * Maximize and Close in the top-right corner.
 */
/**
 * Dragging the list's right edge sets the list's width, saved in settings.json; a double-click
 * goes back to the default. The width can't squeeze the reader below READER_MIN, so what's saved
 * is what was seen. A narrower window later narrows the list too, through the CSS grid.
 */
function initListResize() {
  const handle = $('#list-resize');
  let drag = null;
  handle.addEventListener('pointerdown', (e) => {
    if (e.button !== 0) return;
    e.preventDefault();
    handle.setPointerCapture(e.pointerId);
    drag = { x: e.clientX, width: $('#list-pane').getBoundingClientRect().width, now: null };
    document.documentElement.classList.add('resizing');
  });
  handle.addEventListener('pointermove', (e) => {
    if (!drag) return;
    const room = innerWidth - $('#sidebar').getBoundingClientRect().width - READER_MIN;
    drag.now = Math.round(Math.max(LIST_MIN, Math.min(drag.width + e.clientX - drag.x, LIST_MAX, room)));
    setListWidth(drag.now);
  });
  const end = () => {
    if (!drag) return;
    document.documentElement.classList.remove('resizing');
    if (drag.now) saveListWidth(drag.now);
    drag = null;
  };
  handle.addEventListener('pointerup', end);
  handle.addEventListener('pointercancel', end);
  handle.addEventListener('dblclick', () => {
    setListWidth(null);
    saveListWidth(null);
  });
}

/** `width` in px, or null for the CSS default. */
function setListWidth(width) {
  if (width) $('#app').style.setProperty('--list-width', `${width}px`);
  else $('#app').style.removeProperty('--list-width');
}

function saveListWidth(width) {
  invoke('set_list_width', { width }).catch((e) => toast(String(e)));
}

function initTitleBar() {
  if (!IS_MAC && !navigator.userAgent.includes('Windows')) return;
  const root = document.documentElement;
  root.classList.add('own-title-bar');
  root.classList.toggle('mac', IS_MAC);
  // The strip above the message: on Windows #window-bar, which holds the window buttons; on
  // macOS the message's own button row, level with the list header and the window buttons.
  const strip = IS_MAC ? $('.msg-actions') : $('#window-bar');
  // "deep": anywhere inside drags, except on buttons.
  for (const el of [$('.brand'), $('.list-head'), strip]) el.dataset.tauriDragRegion = 'deep';
  if (IS_MAC) {
    // In full screen the system hides its window buttons, so the brand moves up into their room.
    // macos.rs says when that changes; a page that loads in full screen asks once.
    const fullScreen = (on) => root.classList.toggle('full-screen', on);
    window.__TAURI__.window.getCurrentWindow().isFullscreen().then(fullScreen);
    listen('full-screen', (e) => fullScreen(e.payload));
    return;
  }
  $('#window-bar').hidden = false;
  const win = window.__TAURI__.window.getCurrentWindow();
  $('#win-min').addEventListener('click', () => win.minimize());
  $('#win-max').addEventListener('click', () => win.toggleMaximize());
  $('#win-close').addEventListener('click', () => win.close());
  const showMaximized = async () => {
    const maximized = await win.isMaximized();
    const button = $('#win-max');
    button.textContent = maximized ? '' : '';
    button.title = maximized ? 'Restore Down' : 'Maximize';  // Windows' own names
    button.setAttribute('aria-label', button.title);
  };
  showMaximized();
  win.onResized(showMaximized);
  // The buttons dim while another window is in front, as Windows' own do. This follows the
  // window's focus, not the page's: the page also blurs when a click goes into the email.
  win.onFocusChanged(({ payload: focused }) => root.classList.toggle('inactive', !focused));
}

function bindEvents() {
  $('#refresh').addEventListener('click', () => refresh());
  $('#add-account').addEventListener('click', addAccount);
  $('#settings-add').addEventListener('click', addAccount);
  $('#empty-add').addEventListener('click', addAccount);
  $('#open-settings').addEventListener('click', openSettings);
  $('#close-settings').addEventListener('click', closeSettings);
  $('#cancel-wait').addEventListener('click', () => { state.authSeq++; $('#waiting').hidden = true; });
  $('#archive').addEventListener('click', () => act('archive'));
  $('#trash').addEventListener('click', () => act('trash'));
  $('#restore').addEventListener('click', () => act(listName() === 'trash' ? 'restore' : 'unarchive'));
  $('#search-open').title = IS_MAC ? 'Search (/ or ⌘F)' : 'Search (/ or Ctrl+F)';
  $('#search-open').addEventListener('click', openSearch);
  $('#search-close').addEventListener('click', closeSearch);
  // A click on the field's icon or edge goes to the field too.
  $('#search-box').addEventListener('mousedown', (e) => {
    if (e.target.closest('input, button')) return;
    e.preventDefault();
    $('#search-input').focus();
  });
  $('#search-input').addEventListener('input', () => {
    if (!state.search) return;
    clearTimeout(state.search.timer);
    state.search.timer = setTimeout(() => runSearch(), SEARCH_DELAY_MS);
  });
  $('#search-input').addEventListener('keydown', onSearchKey);
  $('#toggle-read').addEventListener('click', () => {
    const m = openMessage();
    if (m) setUnread(m, !m.unread);
  });
  $('#colors').addEventListener('click', () => {
    state.originalColors = !state.originalColors;
    renderActions();
    if (state.full) showBody(state.full);
  });
  // Appearance (or the system) switched between light and dark: redraw the open email.
  matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => {
    renderActions();
    if (state.full) showBody(state.full);
  });
  $('#toast-action').addEventListener('click', runToastAction);
  $('#update').addEventListener('click', installUpdate);
  for (const button of document.querySelectorAll('#theme button')) {
    button.addEventListener('click', () => setTheme(button.dataset.theme));
  }
  $('#client-form').addEventListener('submit', saveClient);
  $('#client-id').addEventListener('paste', pasteClientJson);

  $('#messages').addEventListener('click', (e) => {
    const row = e.target.closest('li.msg');
    if (!row) return;
    const button = e.target.closest('.row-actions button');
    if (button) onRowAction(row, button.dataset.kind);
    else select(row.dataset.key);
  });
  $('#messages').addEventListener('scroll', () => { hideMenu(); maybeLoadOlder(); }, { passive: true });
  document.querySelectorAll('a[data-href]').forEach((a) =>
    a.addEventListener('click', (e) => { e.preventDefault(); openExternal(a.dataset.href); }));
  document.addEventListener('keydown', onKey);
  document.addEventListener('contextmenu', (e) => onContextMenu(e));
  document.addEventListener('mousedown', (e) => { if (!e.target.closest('#menu')) hideMenu(); });
  // Also fires when a click moves focus into the email frame.
  window.addEventListener('blur', hideMenu);
  window.addEventListener('resize', hideMenu);
  // A wider or narrower reader (resizing the window) changes how much an email must shrink.
  new ResizeObserver(() => {
    const frame = $('#m-body iframe');
    if (frame) fitToFrame(frame);
  }).observe($('#m-body'));
}

// ---------- Sync ----------

/** Syncs the Inbox (it drives the unread counts) and the folder on screen. */
async function refresh() {
  if (!state.accounts.length) return;
  if (state.syncing) { state.syncQueued = true; return; }
  state.syncing = true;
  $('#refresh').classList.add('spin');
  setStatus('Syncing…');
  const folders = [...new Set(['inbox', state.folder])];
  try {
    const results = await Promise.all(folders.map((folder) => invoke('refresh_folder', { folder })));
    folders.forEach((folder, i) => {
      applySync(folder, results[i]);
      state.synced[folder] = true;
    });
    notifyNewMail(results[0]);  // folders[0] is the Inbox
    state.lastSync = Date.now();
    // Every sync covers every account, so this is the full list of expired sign-ins.
    const errors = results.flatMap((r) => r.errors);
    state.expired = new Set(errors.filter((e) => e.expired).map((e) => e.account));
    render();
    setStatus('');
    // An expired sign-in shows as Reconnect above the list. Other problems come from each
    // folder synced; show them once, and never over Undo.
    const messages = new Set(errors.filter((e) => !e.expired).map((e) => `${e.account}: ${e.message}`));
    if (messages.size && !state.undo) toast([...messages].join('\n'), 8000);
  } catch (e) {
    setStatus('');
    toast(String(e));
  } finally {
    state.syncing = false;
    $('#refresh').classList.remove('spin');
    if (state.syncQueued) { state.syncQueued = false; refresh(); }
  }
}

/** Stores a folder's messages (or the search's), re-applying changes Gmail hasn't confirmed yet. */
function setList(folder, messages) {
  let list = messages;
  for (const [key, move] of state.pending) {
    if (leaves(move, folder)) list = list.filter((m) => keyOf(m) !== key);
    if (move.join.includes(folder) && !list.some((m) => keyOf(m) === key)) list = withMessage(list, move.message);
  }
  for (const m of list) {
    const change = state.pendingRead.get(keyOf(m));
    if (change) m.unread = change.unread;
  }
  state.lists[folder] = list;
  state.loaded[folder] = true;
}

function withMessage(list, m) {
  return [...list, m].sort(byDate);
}

/** Stores a sync of each account's newest page, keeping older mail that scrolling loaded. */
function applySync(folder, result) {
  const paging = state.paging[folder];
  let list = result.messages;
  for (const account of state.accounts) {
    const page = result.messages.filter((m) => m.account === account);
    const pageOldest = Math.min(...page.map((m) => m.date));  // Infinity if empty
    // An account whose sync failed got its cached page back; it keeps its paging.
    const synced = Boolean(result.more) && account in result.more;
    const more = synced ? result.more[account] : Boolean(paging[account]?.more);
    if (more && paging[account] && paging[account].frontier < pageOldest) {
      // Mail below this page was loaded earlier; keep it, and the deeper frontier.
      list = list.concat(state.lists[folder].filter((m) => m.account === account && m.date < pageOldest));
    } else {
      paging[account] = { frontier: pageOldest, more };
    }
    if (folder === 'inbox' && synced) forgetLeftInbox(account, page, pageOldest, more);
  }
  setList(folder, list.sort(byDate));
}

/** Drops `inboxExtra` entries that an Inbox sync shows aren't in the Inbox any more. */
function forgetLeftInbox(account, page, pageOldest, more) {
  const inPage = new Set(page.map(keyOf));
  for (const [key, date] of state.inboxExtra) {
    // The page covers everything down to its oldest message, or all of the Inbox if no more.
    if (key.startsWith(account + '/') && (date >= pageOldest || !more) && !inPage.has(key)) {
      state.inboxExtra.delete(key);
    }
  }
}

/**
 * Loads the next older page for the account(s) holding the list back, while near the bottom:
 * the folder's, or the search's results.
 */
async function loadOlder() {
  const folder = listName();
  const accounts = accountsWithMore(folder);
  if (!accounts.length) return;
  const frontier = Math.max(...accounts.map((a) => state.paging[folder][a].frontier));
  const targets = accounts.filter((a) => state.paging[folder][a].frontier === frontier);
  const { search } = state;
  const seq = search?.seq;
  state.loadingOlder = true;
  try {
    const pages = await Promise.all(targets.map((account) => folder === 'search'
      ? invoke('search_messages', { account, query: search.query, before: frontier })
      : invoke('load_older', { folder, account, before: frontier })));
    // The search was closed or changed while these loaded.
    if (folder === 'search' && (state.search !== search || search.seq !== seq)) return;
    targets.forEach((account, i) => mergeOlder(folder, account, pages[i]));
    const warnings = pages.map((p) => p.warning).filter(Boolean);
    if (warnings.length && !state.undo) toast(warnings.join('\n'), 8000);
  } catch (e) {
    state.olderRetryAt = Date.now() + 15000;
    if (!state.undo) toast(`Couldn't load older messages: ${e}`, 6000);
  } finally {
    state.loadingOlder = false;
    render();  // keeps loading while the list is still scrolled to the bottom
  }
}

function mergeOlder(folder, account, page) {
  const paging = state.paging[folder][account];
  const known = new Set(state.lists[folder].map(keyOf));
  const fresh = page.messages.filter((m) => {
    const move = state.pending.get(keyOf(m));
    return !known.has(keyOf(m)) && !(move && leaves(move, folder));
  });
  const oldest = Math.min(paging.frontier, ...page.messages.map((m) => m.date));
  // Stop if Gmail returned nothing older, so a quirk can't make this loop forever.
  state.paging[folder][account] = { frontier: oldest, more: page.more && oldest < paging.frontier };
  if (folder === 'all' || folder === 'search') noteInInbox(page);
  state.lists[folder] = state.lists[folder].concat(fresh).sort(byDate);
}

/** Notes which of an All Mail page's messages (or search results) are in the Inbox. */
function noteInInbox(page) {
  const inInboxIds = new Set(page.inbox);
  for (const m of page.messages) if (inInboxIds.has(m.id)) state.inboxExtra.set(keyOf(m), m.date);
}

/** Starts loading older mail when the list is scrolled (or short enough) to its end. */
function maybeLoadOlder() {
  const list = $('#messages');
  if (state.loadingOlder || list.hidden || Date.now() < state.olderRetryAt) return;
  if (list.scrollTop + list.clientHeight < list.scrollHeight - 400) return;
  if (accountsWithMore(listName()).length) loadOlder();
}

// ---------- New mail: notifications and the icon badge ----------

/**
 * Notifies about Inbox mail newer than any seen before for its account, if it's unread and
 * the window isn't in front. An account's first sync (at launch, or after adding it) only
 * sets the mark, so mail that's already there doesn't notify, nor does older mail that
 * moves back into the Inbox.
 */
function notifyNewMail(result) {
  const fresh = [];
  for (const account of state.accounts) {
    if (!result.more || !(account in result.more)) continue;  // its sync failed
    const page = result.messages.filter((m) => m.account === account);
    const seen = state.newestSeen[account];
    if (seen !== undefined) fresh.push(...page.filter((m) => m.unread && m.date > seen));
    state.newestSeen[account] = Math.max(seen ?? 0, ...page.map((m) => m.date));
  }
  if (!fresh.length || document.hasFocus()) return;
  fresh.sort((a, b) => a.date - b.date);  // the newest ends up on top
  const withAccount = state.accounts.length > 1;
  const notes = fresh.length <= MAX_NOTIFICATIONS
    ? fresh.map((m) => ({
      title: senderName(m.from),
      body: (m.subject || '(no subject)') + (withAccount ? `\n${m.account}` : ''),
      account: m.account,
      id: m.id,
    }))
    : [{ title: `${fresh.length} new messages`, body: [...new Set(fresh.map((m) => senderName(m.from)))].join(', ') }];
  for (const note of notes) invoke('notify', note).catch((e) => console.warn(e));
}

/** A click on a notification: show the Inbox, and open the message if it names one. */
async function openFromNotification(message) {
  closeSearch();
  if (state.folder !== 'inbox') await openFolder('inbox');
  if (state.filter && (!message || state.filter !== message.account)) setFilter(null);
  if (message && byKey(keyOf(message))) select(keyOf(message));
}

/** Shows the Inbox's unread count, all accounts together, on the taskbar / Dock icon. */
function renderBadge(count) {
  if (count === state.badge) return;
  state.badge = count;
  const size = Math.min(32, Math.max(16, Math.round(16 * devicePixelRatio)));
  invoke('set_badge', { count, icon: count ? badgeIcon(count, size) : [], size }).catch((e) => console.warn(e));
}

/** A red disc with the count, as RGBA bytes: the taskbar overlay icon on Windows. */
function badgeIcon(count, size) {
  const canvas = document.createElement('canvas');
  canvas.width = canvas.height = size;
  const g = canvas.getContext('2d');
  g.fillStyle = '#d93025';
  g.beginPath();
  g.arc(size / 2, size / 2, size / 2, 0, 2 * Math.PI);
  g.fill();
  g.fillStyle = '#fff';
  g.font = `bold ${Math.round(size * 0.62)}px "Segoe UI", system-ui, sans-serif`;
  g.textAlign = 'center';
  g.textBaseline = 'middle';
  // Wider counts are squeezed to fit the disc.
  g.fillText(count > 99 ? '99+' : String(count), size / 2, size * 0.54, size * 0.8);
  return Array.from(g.getImageData(0, 0, size, size).data);
}

/** Shows a folder chosen in the sidebar, closing the search if it's open. */
async function openFolder(folder) {
  closeSearch();
  if (folder !== state.folder) {
    state.folder = folder;
    clearReader();
    if (!state.loaded[folder]) setList(folder, await invoke('cached_folder', { folder }));
    render();
  }
  refresh();
}

// ---------- Rendering ----------

function render() {
  renderSidebar();
  renderList();
  renderSearch();
  // An open message's buttons depend on the lists (e.g. Archive vs Move to Inbox in All Mail).
  renderActions();
  // A short list (or one already scrolled to the end) loads older mail right away.
  maybeLoadOlder();
}

function renderSidebar() {
  const unread = new Map();
  for (const m of state.lists.inbox) if (m.unread) unread.set(m.account, (unread.get(m.account) || 0) + 1);
  const inboxUnread = state.filter
    ? unread.get(state.filter) || 0
    : [...unread.values()].reduce((a, b) => a + b, 0);

  $('#folders').replaceChildren(...Object.entries(FOLDERS).map(([folder, f]) => navItem({
    label: f.name,
    icon: f.icon,
    count: folder === 'inbox' ? inboxUnread : 0,
    active: state.folder === folder,
    onClick: () => openFolder(folder),
  })));
  $('#accounts').replaceChildren(
    navItem({ label: 'All accounts', active: state.filter === null, onClick: () => setFilter(null) }),
    ...state.accounts.map((account) => navItem({
      label: account,
      color: colorOf(account),
      count: unread.get(account) || 0,
      expired: state.expired.has(account),
      active: state.filter === account,
      onClick: () => setFilter(account),
    })),
  );
  const title = FOLDERS[state.folder].name + (state.filter ? ` · ${state.filter}` : '');
  $('#list-title').textContent = title;
  $('#list-title').title = title;
  renderSettingsAccounts();
  renderReconnect();
  renderBadge([...unread.values()].reduce((a, b) => a + b, 0));
}

/**
 * A sidebar entry: a folder (`icon`), an account (`color`), or "All accounts" (neither).
 * An `expired` account shows "!" instead of its count.
 */
function navItem({ label, icon, color, count = 0, expired = false, active, onClick }) {
  const button = document.createElement('button');
  button.className = 'nav-item' + (active ? ' active' : '') + (icon || color ? '' : ' all') + (expired ? ' expired' : '');
  button.innerHTML = (icon ? `<svg viewBox="0 0 24 24" aria-hidden="true"><path d="${icon}"/></svg>` : '<span class="dot"></span>') +
    '<span class="label"></span><span class="count"></span>';
  if (color) button.style.setProperty('--acc', color);
  button.querySelector('.label').textContent = label;
  button.querySelector('.count').textContent = expired ? '!' : count || '';
  button.title = expired ? `${label}: sign-in expired, reconnect it above the message list` : label;
  button.addEventListener('click', onClick);
  return button;
}

/** One line per expired account above the message list, each with a Reconnect button. */
function renderReconnect() {
  const expired = state.accounts.filter((account) => state.expired.has(account));
  $('#reconnect').hidden = !expired.length;
  $('#reconnect').replaceChildren(...expired.map((account) => {
    const row = document.createElement('div');
    row.className = 'reconnect-row';
    row.innerHTML = '<span></span><button>Reconnect</button>';
    row.querySelector('span').textContent = `Sign-in expired for ${account}`;
    row.querySelector('span').title = account;
    row.querySelector('button').addEventListener('click', () => signIn(account));
    return row;
  }));
}

function setFilter(account) {
  state.filter = account;
  render();
}

// Next to the time of a message with files (`hasFiles`, which Rust tells from its parts).
const CLIP_ICON = '<svg class="clip" viewBox="0 0 24 24" role="img" aria-label="Has attachments">' +
  '<path transform="rotate(45 12 12)" d="M16.5 7v8.5a4.5 4.5 0 0 1-9 0V7a3 3 0 0 1 6 0v8.5a1.5 1.5 0 0 1-3 0V9"/></svg>';

function renderList() {
  const folder = listName();
  const messages = visible();
  // In All Mail and search results, tag what's still in the Inbox (everything else there is
  // archived or sent).
  const inboxKeys = folder === 'all' || folder === 'search'
    ? new Set([...state.lists.inbox.map(keyOf), ...state.inboxExtra.keys()])
    : null;
  const marks = folder === 'search' ? searchPattern(state.search.query) : null;
  const fragment = document.createDocumentFragment();
  for (const m of messages) {
    const li = document.createElement('li');
    li.className = 'msg' + (m.unread ? ' unread' : '') + (keyOf(m) === state.selected ? ' selected' : '');
    li.dataset.key = keyOf(m);
    li.style.setProperty('--acc', colorOf(m.account));
    li.innerHTML = `<div class="row1"><span class="from"></span>${m.hasFiles ? CLIP_ICON : ''}<time></time></div>` +
      '<div class="subject"><span class="text"></span></div><div class="snippet"></div>';
    setMarkedText(li.querySelector('.from'), senderName(m.from), marks);
    li.querySelector('time').textContent = shortDate(m.date);
    setMarkedText(li.querySelector('.subject .text'), m.subject || '(no subject)', marks);
    if (inboxKeys && inboxKeys.has(keyOf(m))) {
      const tag = document.createElement('span');
      tag.className = 'tag';
      tag.textContent = 'Inbox';
      li.querySelector('.subject').prepend(tag);
    }
    setMarkedText(li.querySelector('.snippet'), m.snippet, marks);
    li.title = m.account;
    li.append(rowActions(m, inboxKeys));
    fragment.append(li);
  }
  if (messages.length && accountsWithMore(folder).length) {
    const more = document.createElement('li');
    more.className = 'more';
    more.textContent = folder === 'search' ? 'Loading older results…' : 'Loading older messages…';
    fragment.append(more);
  }
  $('#messages').replaceChildren(fragment);

  const empty = messages.length === 0;
  $('#list-empty').hidden = !empty;
  $('#messages').hidden = empty;
  if (empty) {
    const noAccounts = state.accounts.length === 0;
    let text;
    if (noAccounts) text = 'Add your Gmail account to get started.';
    else if (folder === 'search') text = state.search.done ? `Nothing matches “${state.search.query}”.` : 'Searching…';
    else text = state.syncing && !state.synced[folder] ? 'Loading…' : FOLDERS[folder].empty;
    $('#list-empty-text').textContent = text;
    $('#empty-add').hidden = !noAccounts;
  }
}

/**
 * The words of a search to mark in its results, as a pattern, or null if there are none: what
 * was typed, without Gmail's operators (`from:`, `has:attachment`, `-word`, `OR`...). Quoted words
 * count as one. Single letters aren't marked, since they'd be marked everywhere.
 */
function searchPattern(query) {
  const words = [];
  for (const [, quoted, word] of query.matchAll(/"([^"]*)"?|(\S+)/g)) {
    if (word && (word.includes(':') || word.startsWith('-') || /^(OR|AND|AROUND)$/.test(word))) continue;
    const text = (quoted ?? word.replace(/["(){}]/g, '').replace(/^\+/, '')).trim();
    if (text.length > 1) words.push(text);
  }
  if (!words.length) return null;
  // Longest first, so a longer word wins where two overlap.
  words.sort((a, b) => b.length - a.length);
  return new RegExp(words.map((w) => w.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('|'), 'giu');
}

/** Sets an element's text, marking what `marks` (from searchPattern) matches. */
function setMarkedText(el, text, marks) {
  if (!marks) {
    el.textContent = text;
    return;
  }
  el.replaceChildren();
  let at = 0;
  for (const match of text.matchAll(marks)) {
    const mark = document.createElement('mark');
    mark.textContent = match[0];
    el.append(text.slice(at, match.index), mark);
    at = match.index + match[0].length;
  }
  el.append(text.slice(at));
}

/** The buttons a row shows under the mouse: the folder's actions, then Mark as read/unread. */
function rowActions(m, inboxKeys) {
  const actions = $('#row-actions').content.firstElementChild.cloneNode(true);
  const allowed = actionsFor(m, inboxKeys);
  for (const button of [...actions.children]) {
    const kind = button.dataset.kind;
    const offered = kind === 'read' ||
      allowed.includes(kind) || (kind === 'restore' && allowed.includes('unarchive'));
    if (!offered) button.remove();
  }
  setReadButton(actions.querySelector('[data-kind="read"]'), m.unread);
  return actions;
}

function setReadButton(button, unread) {
  const title = unread ? 'Mark as read (Shift+I)' : 'Mark as unread (Shift+U)';
  button.title = title;
  button.setAttribute('aria-label', title.replace(/ \(.*/, ''));
}

/** A click on one of a row's buttons acts on that row's message, as its right-click menu does. */
function onRowAction(row, kind) {
  const m = byKey(row.dataset.key);
  if (!m) return;
  if (kind === 'read') setUnread(m, !m.unread);
  else act(kind === 'restore' && listName() !== 'trash' ? 'unarchive' : kind, m);
}

function markRow(key) {
  for (const li of $('#messages').children) {
    li.classList.toggle('selected', li.dataset.key === key);
  }
  const row = $('#messages').querySelector('li.selected');
  if (row) row.scrollIntoView({ block: 'nearest' });
}

function renderActions() {
  const m = openMessage();
  const allowed = m ? actionsFor(m) : [];
  $('#archive').hidden = !allowed.includes('archive');
  $('#trash').hidden = !allowed.includes('trash');
  $('#restore').hidden = !allowed.includes('restore') && !allowed.includes('unarchive');
  const unread = Boolean(m && m.unread);
  $('#toggle-read .label').textContent = unread ? 'Mark as read' : 'Mark as unread';
  $('#toggle-read').title = unread ? 'Mark as read (Shift+I)' : 'Mark as unread (Shift+U)';
  const original = state.originalColors;
  $('#colors').hidden = !m || !appIsDark();
  $('#colors .sun').style.display = original ? 'none' : '';
  $('#colors .moon').style.display = original ? '' : 'none';
  $('#colors .label').textContent = original ? 'Dark' : 'Original';
  $('#colors').title = original ? 'Show this email in dark colors' : "Show this email's original colors";
}

// ---------- Search ----------

/** Opens the search field in the list's header: the magnifier, `/`, or Cmd+F (Ctrl+F on Windows). */
function openSearch() {
  if (!state.accounts.length) return;
  if (!state.search) {
    state.search = { query: '', seq: 0, done: false, timer: null, scroll: $('#messages').scrollTop };
    $('#search-input').value = '';
    render();
  }
  $('#search-input').focus();
  $('#search-input').select();
}

/** Closes the search and shows the folder again, scrolled where it was. */
function closeSearch() {
  const search = state.search;
  if (!search) return;
  clearTimeout(search.timer);
  state.search = null;
  clearSearchResults();
  // A result that isn't in the folder closes with the search.
  if (state.current && !byKey(keyOf(state.current))) clearReader();
  if (document.activeElement === $('#search-input')) $('#search-input').blur();
  render();
  $('#messages').scrollTop = search.scroll;
}

function clearSearchResults() {
  state.lists.search = [];
  state.loaded.search = false;
  state.paging.search = {};
  state.searchLeft.clear();
}

/**
 * Searches every account for what's in the field, with Gmail's search (`search_messages`): its
 * All Mail, newest first, 50 per account, more as the results scroll (loadOlder). An account
 * picked in the sidebar only narrows what's shown, so switching accounts doesn't search again.
 * `again`: search even if the field hasn't changed (Enter), for mail that arrived since.
 */
async function runSearch(again = false) {
  const search = state.search;
  if (!search) return;
  clearTimeout(search.timer);
  const query = $('#search-input').value.trim();
  if (query === search.query && !again) return;
  const seq = ++search.seq;
  search.query = query;
  search.done = false;
  clearSearchResults();
  render();
  // An emptied field shows the folder again, where it was.
  $('#messages').scrollTop = query ? 0 : search.scroll;
  if (!query) return;
  // Expired sign-ins can't search; the line above the list already offers Reconnect for them.
  const accounts = state.accounts.filter((account) => !state.expired.has(account));
  const answers = await Promise.all(accounts.map((account) =>
    invoke('search_messages', { account, query, before: null })
      .then((page) => ({ account, page }), (error) => ({ account, error }))));
  if (state.search !== search || search.seq !== seq) return;  // closed, or searching for something else now
  const messages = [];
  const problems = [];
  for (const { account, page, error } of answers) {
    if (error) {
      problems.push(`Couldn't search ${account}: ${error}`);
      continue;
    }
    messages.push(...page.messages);
    state.paging.search[account] = { frontier: Math.min(...page.messages.map((m) => m.date)), more: page.more };
    noteInInbox(page);
    if (page.warning) problems.push(`${account}: ${page.warning}`);
  }
  setList('search', messages.sort(byDate));
  search.done = true;
  render();
  if (problems.length && !state.undo) toast(problems.join('\n'), 8000);
}

/** Keys in the search field: Enter searches at once, Esc closes, ↓ goes to the results. */
function onSearchKey(e) {
  if (e.isComposing) return;  // choosing characters in an input method
  if (e.key === 'Enter') {
    e.preventDefault();
    runSearch(true);
  } else if (e.key === 'Escape') {
    e.preventDefault();
    closeSearch();
  } else if (e.key === 'ArrowDown' && visible().length) {
    e.preventDefault();
    $('#search-input').blur();
    if (!visible().some((m) => keyOf(m) === state.selected)) move(1);
  }
}

/** The search field in place of the list's header, and the line under it: where it searches, or what it found. */
function renderSearch() {
  const search = state.search;
  $('#search-open').hidden = !state.accounts.length;
  $('.list-head').classList.toggle('searching', Boolean(search));
  $('#search-scope').hidden = !search;
  if (!search) return;
  const where = state.filter || state.accounts.length === 1 ? `in ${state.filter || state.accounts[0]}` : 'in all accounts';
  let text;
  if (!search.query) text = `Search All Mail ${where}`;
  else if (!search.done) text = `Searching ${where}…`;
  else {
    const count = visible().length;
    const more = accountsWithMore('search').length > 0;
    text = count ? `${count}${more ? '+' : ''} ${count === 1 && !more ? 'result' : 'results'} ${where}` : `No results ${where}`;
  }
  $('#search-scope').textContent = text;
}

// ---------- Reading ----------

async function select(key) {
  const m = byKey(key);
  if (!m) return;
  state.selected = key;
  state.current = m;
  state.full = null;
  state.originalColors = false;  // each email starts dark in dark mode
  markRow(key);
  renderActions();

  $('#reader-empty').hidden = true;
  $('#message').hidden = false;
  $('#m-subject').textContent = m.subject || '(no subject)';
  $('#m-from').textContent = m.from;
  $('#m-date').textContent = longDate(m.date);
  $('#m-to').textContent = '';
  $('#m-account').textContent = m.account;
  $('#m-account').style.setProperty('--acc', colorOf(m.account));
  renderAttachments(m, []);
  showBodyMessage('Loading…');

  const seq = ++state.openSeq;
  try {
    const full = await invoke('get_message', { account: m.account, id: m.id });
    if (seq !== state.openSeq) return;
    $('#m-to').textContent = 'to ' + [full.to, full.cc].filter(Boolean).join(', ');
    full.inline = inlinePictures(full);
    renderAttachments(m, (full.attachments || []).filter((a) => !full.inline.includes(a)));
    showBody(full);
    if (full.inline.length) loadInlinePictures(m, full, seq);
    // Opening marks it read, as in Gmail.
    if ((byKey(key) || m).unread) setUnread(m, false);
  } catch (e) {
    if (seq === state.openSeq) showBodyMessage(`Couldn't load this message: ${e}`);
  }
}

function clearReader() {
  state.selected = null;
  state.current = null;
  state.full = null;
  state.openSeq++;
  $('#message').hidden = true;
  $('#reader-empty').hidden = false;
  $('#m-body').replaceChildren();
}

// ---------- Attachments ----------

const FILE_ICON = '<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5"/>';
const ATTACHMENT_ICONS = {
  image: '<rect x="3" y="4" width="18" height="16" rx="2"/><circle cx="9" cy="10" r="2"/><path d="M21 16l-5-5-9 9"/>',
  pdf: FILE_ICON + '<path d="M9 13h6M9 17h4"/>',
  archive: FILE_ICON + '<path d="M10 7h2M10 10h2M10 13h2M10 16h2"/>',
  document: FILE_ICON + '<path d="M9 13h6M9 17h6"/>',
  file: FILE_ICON,
};
const SAVE_ICON = '<svg class="save" viewBox="0 0 24 24" aria-hidden="true"><path d="M12 4v11M7 10l5 5 5-5M5 20h14"/></svg>';
// While a save button saves, in place of the save icon: a click there cancels.
const CANCEL_ICON = '<svg class="cancel" viewBox="0 0 24 24" aria-hidden="true"><path d="M7 7l10 10M17 7 7 17"/></svg>';
// What a save button says while it saves, in one grid cell (`save-label`) with its usual label
// (`idle`): how much has arrived and, unseen, the finished progress text (`sizer`), which keeps
// the cell one width throughout. The CSS picks what shows.
const SAVING_LABELS = '<span class="progress"></span><span class="sizer" aria-hidden="true"></span>';
let lastSaveJob = 0;

/** The open message's files, under its header: a chip each, and Save all for two or more. */
function renderAttachments(m, attachments) {
  const box = $('#m-attachments');
  box.replaceChildren(...attachments.map((a) => attachmentChip(m, a)));
  if (attachments.length > 1) {
    const all = document.createElement('button');
    all.className = 'save-all';
    all.innerHTML = `<span class="save-label"><span class="idle">Save all</span>${SAVING_LABELS}</span>${CANCEL_ICON}`;
    all.title = 'Save all attachments to Downloads';
    all.addEventListener('click', () => saveAttachments(m, attachments, all));
    box.append(all);
  }
  box.hidden = !attachments.length;
}

function attachmentChip(m, a) {
  const kind = attachmentKind(a);
  const chip = document.createElement('button');
  chip.className = `att ${kind}`;
  chip.title = `Save ${a.name} to Downloads`;
  chip.innerHTML = `<svg class="kind" viewBox="0 0 24 24" aria-hidden="true">${ATTACHMENT_ICONS[kind]}</svg>` +
    `<span class="name"></span><span class="save-label"><span class="size idle"></span>${SAVING_LABELS}</span>${SAVE_ICON}${CANCEL_ICON}`;
  chip.querySelector('.name').textContent = a.name;
  chip.querySelector('.size').textContent = formatSize(a.size);
  chip.addEventListener('click', () => saveAttachments(m, [a], chip));
  return chip;
}

/** Picks the icon and its color, from the type Gmail gives or, for `octet-stream`, the extension. */
function attachmentKind(a) {
  const type = a.mimeType;
  const extension = a.name.includes('.') ? a.name.split('.').pop().toLowerCase() : '';
  if (type.startsWith('image/')) return 'image';
  if (type === 'application/pdf' || extension === 'pdf') return 'pdf';
  if (/zip|rar|7z|tar|compressed/.test(type) || ['zip', 'rar', '7z', 'gz', 'tgz', 'tar', 'xz', 'bz2'].includes(extension)) return 'archive';
  if (/^text\/|word|excel|spreadsheet|presentation|powerpoint|opendocument|rtf/.test(type) ||
    ['doc', 'docx', 'xls', 'xlsx', 'ppt', 'pptx', 'odt', 'ods', 'odp', 'rtf', 'txt', 'csv', 'pages', 'numbers', 'key'].includes(extension)) return 'document';
  return 'file';
}

/** 512 B, 248 KB, 1.2 MB, 24 MB: in units of 1024, as Gmail and Windows count. */
function formatSize(bytes) {
  if (bytes < 1024) return `${bytes} B`;
  const units = ['KB', 'MB', 'GB'];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  return `${unit && value < 10 ? value.toFixed(1) : Math.round(value)} ${units[unit]}`;
}

/**
 * Saves files to Downloads (`save_attachments`); the toast can then show them in Finder or
 * Explorer. Meanwhile the button shows how much has arrived, with a cross in place of its save
 * icon, and another click cancels (`cancel_save`).
 */
async function saveAttachments(m, attachments, button) {
  if (button.classList.contains('saving')) {
    invoke('cancel_save', { job: Number(button.dataset.job) }).catch(() => {});
    return;
  }
  const job = ++lastSaveJob;
  button.dataset.job = job;
  const total = attachments.reduce((sum, a) => sum + a.size, 0);
  button.querySelector('.sizer').textContent = formatProgress(total, total);
  showProgress(button, 0, total);
  button.classList.add('saving');
  button.setAttribute('aria-busy', 'true');
  const title = button.title;
  button.title = 'Cancel';
  const what = attachments.length === 1 ? attachments[0].name : 'the attachments';
  // Rust sends how many bytes have arrived, up to ten times a second. One that comes after the
  // save is over is dropped.
  const progress = new Channel();
  progress.onmessage = (received) => {
    if (button.dataset.job === String(job) && button.classList.contains('saving')) showProgress(button, received, total);
  };
  try {
    const paths = await invoke('save_attachments', {
      account: m.account, id: m.id, parts: attachments.map((a) => a.part), job, onProgress: progress,
    });
    if (!paths.length) return;  // cancelled
    const saved = paths.length === 1 ? paths[0].split(/[\\/]/).pop() : `${paths.length} files`;
    toast(`Saved ${saved} to Downloads`, 8000, {
      label: IS_MAC ? 'Show in Finder' : 'Show in folder',
      run: () => invoke('show_in_folder', { paths }).catch((e) => toast(`Couldn't show the file: ${e}`, 6000)),
    });
  } catch (e) {
    toast(`Couldn't save ${what}: ${e}`, 8000);
  } finally {
    button.title = title;
    button.classList.remove('saving');
    button.removeAttribute('aria-busy');
    button.style.removeProperty('--progress');
  }
}

/** Shows how much of a save has arrived: in words, and as the line along the button's bottom. */
function showProgress(button, received, total) {
  button.querySelector('.progress').textContent = formatProgress(received, total);
  button.style.setProperty('--progress', total ? Math.min(received / total, 1) : 0);
}

/** "1.2 of 3.4 MB": `received` in the unit and precision formatSize gives `total`. */
function formatProgress(received, total) {
  const whole = formatSize(total);
  const [number, unit] = whole.split(' ');
  const scale = 1024 ** ['B', 'KB', 'MB', 'GB'].indexOf(unit);
  return `${(Math.min(received, total) / scale).toFixed(number.includes('.') ? 1 : 0)} of ${whole}`;
}

// ---------- Pictures in the email (cid: links) ----------

/**
 * The pictures the email shows in place: attachments with a Content-ID that one of its `cid:`
 * links names. They leave the list of files, as in Gmail.
 */
function inlinePictures(full) {
  if (!full.html) return [];
  const named = new Set([...decodeBody(full.html).matchAll(/cid:([^"'\s)>]+)/gi)].map((m) => cidKey(m[1])));
  return (full.attachments || []).filter((a) =>
    a.contentId && a.mimeType.startsWith('image/') && named.has(cidKey(a.contentId)));
}

/** How `cid:` links and Content-IDs are compared: URL-decoded, ignoring case. */
function cidKey(id) {
  try { id = decodeURIComponent(id); } catch { /* keep it as written */ }
  return id.trim().toLowerCase();
}

/**
 * Fetches the pictures the email shows in place (`inline_images`) once its text is on screen,
 * and points its `cid:` links at them: in the open email, and in later redraws (`full.pictures`,
 * which showBody uses).
 */
async function loadInlinePictures(m, full, seq) {
  try {
    const parts = await invoke('inline_images', { account: m.account, id: m.id, parts: full.inline.map((a) => a.part) });
    if (seq !== state.openSeq) return;
    const pictures = new Map();
    for (const p of parts) {
      const a = full.inline.find((x) => x.part === p.part);
      let data = p.data.replace(/-/g, '+').replace(/_/g, '/');
      while (data.length % 4) data += '=';
      if (a) pictures.set(cidKey(a.contentId), `data:${p.mimeType};base64,${data}`);
    }
    full.pictures = pictures;
    const doc = $('#m-body iframe')?.contentDocument;
    if (doc && doc.URL === 'about:srcdoc') showInlinePictures(doc, pictures);
  } catch (e) {
    if (seq === state.openSeq) toast(`Couldn't load the pictures in this email: ${e}`, 6000);
  }
}

/** Points the email's `cid:` links (`src`, `background`, `url()` in a style) at the pictures' data. */
function showInlinePictures(doc, pictures) {
  const picture = (link) => {
    const match = /^\s*cid:(.+?)\s*$/i.exec(link || '');
    return match ? pictures.get(cidKey(match[1])) : undefined;
  };
  for (const el of doc.querySelectorAll('[src], [background]')) {
    for (const name of ['src', 'background']) {
      const data = picture(el.getAttribute(name));
      if (data) el.setAttribute(name, data);
    }
  }
  for (const el of doc.querySelectorAll('[style*="cid:" i]')) {
    el.setAttribute('style', el.getAttribute('style').replace(/url\(\s*(['"]?)(cid:[^'")]+)\1\s*\)/gi,
      (all, quote, link) => { const data = picture(link); return data ? `url("${data}")` : all; }));
  }
}

// ---------- Message body ----------

function showBodyMessage(text) {
  const div = document.createElement('div');
  div.className = 'empty';
  div.textContent = text;
  $('#m-body').replaceChildren(div);
}

/** Renders email HTML in a sandboxed iframe: no scripts, forms or popups. */
function showBody(full) {
  let html;
  if (full.html) html = decodeBody(full.html);
  else if (full.text) html = `<pre class="plain">${linkify(escapeHtml(decodeBody(full.text)))}</pre>`;
  else html = '<p style="color:#888">(This message has no text.)</p>';

  state.full = full;  // to show it again with other colors (Original / Dark, a theme change)
  const dark = appIsDark() && !state.originalColors;
  const frame = document.createElement('iframe');
  // allow-same-origin (without allow-scripts) only lets this page reach the email to set it up
  // and attach its handlers. WebKit (macOS) doesn't run those handlers inside the frame, so there
  // a link click has to ask for a new window (allow-popups), which macos.rs opens in the browser.
  frame.setAttribute('sandbox', IS_MAC ? 'allow-same-origin allow-popups' : 'allow-same-origin');
  frame.setAttribute('referrerpolicy', 'no-referrer');
  frame.srcdoc = `<!doctype html><html><head><meta charset="utf-8"><base target="_blank">` +
    `<style>${FRAME_CSS}${dark ? FRAME_DARK_CSS : ''}</style></head><body>${html}</body></html>`;
  // On macOS the frame's scroll bar is only a see-through thumb, over the frame's own background.
  // A darkened email gets a dark one, as the thumb is then light.
  if (dark) frame.style.background = EMAIL_DARK_BACKGROUND;
  // In dark mode the email stays invisible until its colors are set, so it doesn't flash white.
  // Not `visibility: hidden`: a frame shown after that doesn't scroll by trackpad or mouse wheel
  // in WebKit (macOS) until something lays it out again, only by dragging its scroll bar.
  if (appIsDark()) {
    frame.style.opacity = '0';
    setTimeout(() => { frame.style.opacity = ''; }, 3000);  // in case setting up never happens
  }
  // Set the email up as soon as it's parsed. Its `load` waits for every image, and one slow
  // tracking pixel can hold that for 20+ s, leaving the email unfitted and unclickable.
  whenParsed(frame, (doc) => {
    if (full.pictures) showInlinePictures(doc, full.pictures);
    if (appIsDark()) setEmailColors(doc, dark);
    frame.style.opacity = '';
    fitToFrame(frame);
    // Every link opens through <base target="_blank">, which the click handler below (or on
    // macOS, macos.rs) catches. A link's own target, such as _top, would get around that.
    for (const link of doc.querySelectorAll('a[target], area[target]')) link.removeAttribute('target');
    // Images can widen the layout as they arrive. Their `load` events don't bubble, so capture them.
    doc.addEventListener('load', () => scheduleFit(frame), true);
    doc.addEventListener('click', (e) => {
      const link = e.target.closest && e.target.closest('a[href]');
      if (!link) return;
      e.preventDefault();
      if (!link.getAttribute('href').startsWith('#')) openExternal(link.href);
    });
    // On macOS keys come from macos.rs instead (onEmailKey), so they never act twice.
    if (!IS_MAC) doc.addEventListener('keydown', (e) => onKey(e, true));
    doc.addEventListener('contextmenu', (e) => onContextMenu(e, frame));
    doc.addEventListener('mousedown', hideMenu);
  });
  // Once everything has loaded (web fonts too), fit one last time.
  frame.addEventListener('load', () => scheduleFit(frame));
  $('#m-body').replaceChildren(frame);
}

/**
 * Calls `ready` with the frame's email document once it's parsed, before its images load.
 * A new frame first holds an empty about:blank document; the email's own replaces it shortly.
 */
// Timers rather than requestAnimationFrame, which pauses while the window is minimized or hidden.
function whenParsed(frame, ready) {
  const check = () => {
    if (!frame.isConnected) return;  // another message was opened meanwhile
    const doc = frame.contentDocument;
    if (doc && doc.URL === 'about:srcdoc' && doc.readyState !== 'loading') ready(doc);
    else setTimeout(check, 10);
  };
  setTimeout(check, 0);
}

/** Fits once shortly, however many images arrive meanwhile. */
function scheduleFit(frame) {
  if (frame.fitScheduled) return;
  frame.fitScheduled = true;
  setTimeout(() => {
    frame.fitScheduled = false;
    fitToFrame(frame);
  }, 50);
}

// ---------- Email colors in dark mode ----------

const appIsDark = () => matchMedia('(prefers-color-scheme: dark)').matches;

/**
 * Colors for an email while the app is dark. Emails with their own dark styles keep them;
 * others are darkened. `dark` false ("Original") shows the email's light design instead.
 * The frame always reports the app's dark preference, so that means turning the email's
 * dark rules off.
 */
function setEmailColors(doc, dark) {
  const ownDark = adjustColorSchemeRules(doc, dark);
  if (dark && !ownDark) darkenEmail(doc);
}

/**
 * Finds the email's own dark-mode rules (`prefers-color-scheme` in its CSS and in
 * `<picture><source media>`) and reports whether it has any. With `keepDark` false they are
 * rewritten to never match, and its light-only rules to always match. Only the CSS changes.
 */
function adjustColorSchemeRules(doc, keepDark) {
  let found = false;
  const adjust = (media) => {
    if (/prefers-color-scheme\s*:\s*dark/i.test(media)) found = true;
    if (keepDark || !/prefers-color-scheme/i.test(media)) return media;
    return media.replace(/prefers-color-scheme\s*:\s*dark/gi, 'max-width: 0px')
      .replace(/prefers-color-scheme\s*:\s*light/gi, 'min-width: 0px');
  };
  const walk = (rules) => {
    for (const rule of rules) {
      if (rule.media) rule.media.mediaText = adjust(rule.media.mediaText);
      if (rule.cssRules) walk(rule.cssRules);
    }
  };
  for (const sheet of doc.styleSheets) {
    try { walk(sheet.cssRules); } catch { /* a stylesheet from another site can't be read */ }
  }
  for (const source of doc.querySelectorAll('source[media]')) {
    source.media = adjust(source.media);
  }
  return found;
}

/**
 * Recolors an email made for white: light backgrounds and borders turn dark and dark text
 * turns light, keeping their hues. Saturated mid tones (buttons, banners) and images stay
 * as they are. Inline !important styles win over the email's own CSS.
 */
function darkenEmail(doc) {
  const view = doc.defaultView;
  for (const el of [doc.documentElement, doc.body, ...doc.body.querySelectorAll('*')]) {
    const style = view.getComputedStyle(el);
    const background = darkerColor(style.backgroundColor);
    if (background) el.style.setProperty('background-color', background, 'important');
    const text = lighterColor(style.color);
    if (text) el.style.setProperty('color', text, 'important');
    for (const side of ['top', 'right', 'bottom', 'left']) {
      if (style.getPropertyValue(`border-${side}-style`) === 'none') continue;
      const border = darkerColor(style.getPropertyValue(`border-${side}-color`));
      if (border) el.style.setProperty(`border-${side}-color`, border, 'important');
    }
  }
}

// Whether a color counts as light or dark goes by its perceived brightness (luminance), not
// HSL lightness: saturated blues like #1a73e8 have 50% lightness but read as dark and strong.

/** A light color (background, border) made dark, or null if it isn't light. */
function darkerColor(css) {
  const c = toHsl(css);
  if (!c || c.a === 0 || c.luminance < 0.4) return null;
  // White becomes 12% lightness, like the frame's dark background; pale tints get muted.
  return hsla(c.h, c.s * 0.6, 0.12 + (1 - c.l) * 0.5, c.a);
}

/** A dark text color made light, or null if it's light already (white text on a button). */
function lighterColor(css) {
  const c = toHsl(css);
  if (!c || c.a === 0 || c.luminance >= 0.3) return null;
  return hsla(c.h, c.s, Math.max(0.92 - c.l * 0.7, 0.6), c.a);
}

/**
 * Parses a computed `rgb()`/`rgba()` color into hue (degrees), saturation, lightness, alpha
 * and relative luminance (0 black to 1 white).
 */
function toHsl(css) {
  const m = /rgba?\(([\d.]+),\s*([\d.]+),\s*([\d.]+)(?:,\s*([\d.]+))?\)/.exec(css || '');
  if (!m) return null;
  const [r, g, b] = [m[1], m[2], m[3]].map((v) => v / 255);
  const max = Math.max(r, g, b), min = Math.min(r, g, b), l = (max + min) / 2;
  let h = 0, s = 0;
  if (max !== min) {
    const d = max - min;
    s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
    h = max === r ? (g - b) / d + (g < b ? 6 : 0) : max === g ? (b - r) / d + 2 : (r - g) / d + 4;
    h *= 60;
  }
  const linear = (v) => (v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4);
  const luminance = 0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b);
  return { h, s, l, a: m[4] === undefined ? 1 : Number(m[4]), luminance };
}

const hsla = (h, s, l, a) => `hsla(${h.toFixed(1)}, ${(s * 100).toFixed(1)}%, ${(l * 100).toFixed(1)}%, ${a})`;

/**
 * Newsletters and receipts are often laid out at a fixed 600-700 px, wider than the reader.
 * Instead of cutting them off with a sideways scroll bar, scale the email down to fit, as
 * Apple Mail and Outlook do. CSS `zoom` changes the layout size, so the scroll bar goes away.
 */
function fitToFrame(frame) {
  const doc = frame.contentDocument;
  if (!doc || !doc.body) return;
  doc.body.style.zoom = '';
  const wide = doc.documentElement.scrollWidth;
  const room = doc.documentElement.clientWidth;
  // One pixel to spare, so rounding can't leave a scroll bar behind.
  if (wide > room) doc.body.style.zoom = String(Math.max(MIN_EMAIL_ZOOM, (room - 1) / wide));
}

/**
 * Gmail body parts are base64url bytes. Gmail may already have converted a part to UTF-8
 * while its header still names the original charset (seen with KOI8-R), so valid UTF-8
 * wins. Text really in a legacy charset (KOI8-R, windows-1251, Latin-1) is almost never
 * valid UTF-8, so it falls through to the declared charset.
 */
function decodeBody(body) {
  const binary = atob(body.data.replace(/-/g, '+').replace(/_/g, '/'));
  const bytes = Uint8Array.from(binary, (c) => c.charCodeAt(0));
  try {
    return new TextDecoder('utf-8', { fatal: true }).decode(bytes);
  } catch { /* not UTF-8: use the declared charset */ }
  let decoder;
  try { decoder = new TextDecoder(body.charset || 'utf-8'); } catch { decoder = new TextDecoder('utf-8'); }
  return decoder.decode(bytes);
}

// ---------- Actions ----------

/**
 * Runs an action on a message (the open one, or a row's from the right-click menu) and
 * offers Undo. If the open message left the folder on screen, the next one opens;
 * otherwise (e.g. Archive in All Mail) it stays open.
 */
function act(kind, m = openMessage()) {
  if (!m || !actionsFor(m).includes(kind)) return;
  const action = ACTIONS[kind];
  const wasOpen = Boolean(state.current) && keyOf(state.current) === keyOf(m);
  const list = visible();
  const index = list.findIndex((x) => keyOf(x) === keyOf(m));
  const next = index >= 0 ? list[index + 1] || list[index - 1] : null;

  const done = perform(m, action, () => action.run(m));
  if (wasOpen && !byKey(keyOf(m))) {
    if (next) select(keyOf(next)); else clearReader();
  }
  done.catch((e) => toast(`Couldn't ${action.verb} the message: ${e}`, 6000));

  toast(action.done, UNDO_MS, {
    label: 'Undo',
    undo: true,
    run: async () => {
      let result;
      try { result = await done; } catch { return; }  // it failed and was already put back
      const undo = action.undo(m, result);
      try {
        await perform(m, undo, undo.run);
        // Reopen the message if it was open and is back in the folder on screen.
        if (wasOpen && byKey(keyOf(m))) select(keyOf(m));
      } catch (e) {
        toast(`Couldn't undo: ${e}`, 6000);
      }
    },
  });
}

/**
 * Moves the message between folder lists right away, then runs `run` on Gmail.
 * Until Gmail answers, syncs keep the move applied; if Gmail fails, the move is reverted.
 */
async function perform(m, move, run) {
  const key = keyOf(m);
  const entry = { message: m, leave: move.leave, join: move.join };
  state.pending.set(key, entry);
  shift(m, move.leave, move.join);
  render();
  try {
    return await run();
  } catch (e) {
    shift(m, move.join, move.leave);
    render();
    throw e;
  } finally {
    if (state.pending.get(key) === entry) state.pending.delete(key);
  }
}

/**
 * Takes the message out of the `leave` lists and adds it to the `join` lists that are loaded.
 * Search results come from All Mail: a message leaving All Mail leaves them too, and one joining
 * All Mail joins them only if it left them before (Undo, or a failed action), since it may not
 * match the search.
 */
function shift(m, leave, join) {
  const key = keyOf(m);
  if (leave.includes('inbox')) state.inboxExtra.delete(key);
  if (join.includes('inbox')) state.inboxExtra.set(key, m.date);
  for (const folder of leave) state.lists[folder] = state.lists[folder].filter((x) => keyOf(x) !== key);
  for (const folder of join) {
    if (state.loaded[folder] && !state.lists[folder].some((x) => keyOf(x) === key)) {
      state.lists[folder] = withMessage(state.lists[folder], m);
    }
  }
  if (leave.includes('all') && state.lists.search.some((x) => keyOf(x) === key)) {
    state.lists.search = state.lists.search.filter((x) => keyOf(x) !== key);
    state.searchLeft.add(key);
  }
  if (join.includes('all') && state.searchLeft.delete(key)) state.lists.search = withMessage(state.lists.search, m);
}

/**
 * Marks a message read or unread: here at once, then at Gmail. Until Gmail answers, syncs
 * keep the new state; if it fails, the old one comes back. Changes to one message reach
 * Gmail in order, so opening it and then marking it unread can't end up read.
 */
function setUnread(m, unread) {
  const key = keyOf(m);
  const before = state.pendingRead.get(key);
  const change = { unread };
  change.done = (before ? before.done.catch(() => {}) : Promise.resolve())
    .then(() => invoke('set_unread', { ...ids(m), unread }));
  state.pendingRead.set(key, change);
  showUnread(key, unread);
  change.done
    .catch((e) => {
      if (state.pendingRead.get(key) === change) showUnread(key, !unread);
      if (!state.undo) toast(`Couldn't mark the message as ${unread ? 'unread' : 'read'}: ${e}`, 6000);
    })
    .finally(() => { if (state.pendingRead.get(key) === change) state.pendingRead.delete(key); });
}

/** Shows a message as read or unread in every list it's in (Inbox and All Mail, say). */
function showUnread(key, unread) {
  for (const list of Object.values(state.lists)) {
    for (const x of list) if (keyOf(x) === key) x.unread = unread;
  }
  if (state.current && keyOf(state.current) === key) state.current.unread = unread;
  const row = $('#messages').querySelector(`li[data-key="${CSS.escape(key)}"]`);
  if (row) {
    row.classList.toggle('unread', unread);
    setReadButton(row.querySelector('.row-actions [data-kind="read"]'), unread);
  }
  renderSidebar();
  renderActions();
}

/** The toast's button: Undo, Show in Finder… */
function runToastAction() {
  const run = state.toastAction;
  if (!run) return;
  hideToast();
  run();
}

/** `Z`: the toast's button, only when it's Undo. */
function runUndo() {
  if (state.undo) runToastAction();
}

function move(delta) {
  const list = visible();
  if (!list.length) return;
  const index = list.findIndex((m) => keyOf(m) === state.selected);
  const nextIndex = index < 0 ? 0 : Math.min(list.length - 1, Math.max(0, index + delta));
  if (nextIndex !== index) select(keyOf(list[nextIndex]));
}

/** Scrolls the open message by about a screen; `direction` is 1 (down) or -1 (up). */
function scrollMessage(direction) {
  const frame = $('#m-body iframe');
  if (frame && frame.contentWindow) frame.contentWindow.scrollBy({ top: direction * frame.clientHeight * 0.9 });
}

/**
 * The key a shortcut sees. With a Latin layout it's the character typed, so Dvorak and AZERTY users
 * get their own J, K, E. With any other layout (Russian, Greek…) the letters and Shift+3 count as
 * the key in the same place on an English keyboard: Russian `о` is J, `№` is #.
 */
function shortcutKey(e) {
  if (/^[a-z#]$/i.test(e.key)) return e.key;
  if (/^Key[A-Z]$/.test(e.code)) {
    const letter = e.code.slice(3);
    return e.shiftKey ? letter : letter.toLowerCase();
  }
  if (e.code === 'Digit3' && e.shiftKey) return '#';
  return e.key;
}

/** `inMessage`: the key was pressed inside the email frame, which scrolls itself. */
function onKey(e, inMessage = false) {
  if (!$('#menu').hidden) {
    hideMenu();
    if (e.key === 'Escape') { e.preventDefault(); return; }
  }
  // Cmd+F (Ctrl+F on Windows) searches, as Find does in most apps.
  if ((IS_MAC ? e.metaKey : e.ctrlKey) && !e.altKey && !e.shiftKey && shortcutKey(e).toLowerCase() === 'f') {
    if ($('#settings').hidden && $('#waiting').hidden) {
      e.preventDefault();
      openSearch();
    }
    return;
  }
  if (e.ctrlKey || e.metaKey || e.altKey) return;
  if (!$('#settings').hidden || !$('#waiting').hidden) {
    if (e.key === 'Escape' && state.configured && state.accounts.length) closeSettings();
    return;
  }
  if (e.target.closest && e.target.closest('input, textarea')) return;
  const key = shortcutKey(e);
  switch (key) {
    case 'j': move(1); break;
    case 'k': move(-1); break;
    case 'ArrowDown': case 'ArrowUp':
      if (inMessage) return;
      move(e.key === 'ArrowDown' ? 1 : -1);
      break;
    case ' ': case 'PageDown': case 'PageUp':
      if (inMessage) return;
      scrollMessage(e.key === 'PageUp' || (e.key === ' ' && e.shiftKey) ? -1 : 1);
      break;
    case 'e': act('archive'); break;
    case '#': case 'Delete': act('trash'); break;
    case 'u': case 'U': case 'i': case 'I': {  // Shift+U / Shift+I, as in Gmail
      if (!e.shiftKey) return;
      const m = openMessage();
      if (m) setUnread(m, key.toLowerCase() === 'u');
      break;
    }
    case 'z': runUndo(); break;
    case '/': openSearch(); break;
    case 'Escape':
      if (!state.search) return;
      closeSearch();
      break;
    default: return;
  }
  e.preventDefault();
}

// ---------- Right-click menu ----------

/**
 * Right-click shows the app's own menu. The webview's menu (Back, Reload, Share, Print…)
 * does nothing useful here and comes in the system's language. Text fields keep it, as
 * their Cut/Copy/Paste menu does in every app. `frame`: the click was in the email.
 */
function onContextMenu(e, frame = null) {
  if (e.target.closest && e.target.closest('input, textarea')) return;
  e.preventDefault();
  const groups = [];
  const row = !frame && e.target.closest && e.target.closest('li.msg');
  const m = row && byKey(row.dataset.key);
  if (m) {
    groups.push(
      [{ label: m.unread ? 'Mark as read' : 'Mark as unread', run: () => setUnread(m, !m.unread) }],
      actionsFor(m).map((kind) => ({ label: ACTIONS[kind].label, danger: kind === 'trash', run: () => act(kind, m) })),
    );
  }
  const link = frame && e.target.closest && e.target.closest('a[href]');
  if (link && /^(https?|mailto):/i.test(link.href)) {
    groups.push([
      { label: 'Open link', run: () => openExternal(link.href) },
      { label: 'Copy link', run: () => copyText(link.href.replace(/^mailto:/i, '')) },
    ]);
  }
  const selection = (frame ? frame.contentWindow : window).getSelection();
  const text = selection.toString();
  if (text.trim() && selection.containsNode(e.target, true)) groups.push([{ label: 'Copy', run: () => copyText(text) }]);

  // Clicks in the email frame are relative to the frame.
  const offset = frame ? frame.getBoundingClientRect() : { left: 0, top: 0 };
  showMenu(groups, e.clientX + offset.left, e.clientY + offset.top, row);
}

/**
 * macOS: a right-click in the email, at `x`, `y` in the window. WebKit doesn't run this page's
 * handlers inside the sandboxed frame, so macos.rs empties WebKit's menu and passes the click on.
 */
function onEmailContextMenu(x, y) {
  const frame = $('#m-body iframe');
  const doc = frame && frame.contentDocument;
  if (!doc) return;
  const box = frame.getBoundingClientRect();
  const target = doc.elementFromPoint(x - box.left, y - box.top);
  if (!target) return;
  onContextMenu({ target, clientX: x - box.left, clientY: y - box.top, preventDefault() {} }, frame);
  // Focus back on the page, so a click in the email moves it there again and closes the menu.
  if (!$('#menu').hidden) window.focus();
}

/** macOS: a key pressed while the email has focus (see onEmailContextMenu). */
function onEmailKey(key, code, shiftKey) {
  const frame = $('#m-body iframe');
  if (!frame || document.activeElement !== frame) return;
  const target = (frame.contentDocument && frame.contentDocument.activeElement) || frame;
  onKey({ key, code, shiftKey, target, preventDefault() {} }, true);
}

/** `groups`: lists of `{ label, run, danger }`, shown with lines between them. */
function showMenu(groups, x, y, row = null) {
  hideMenu();
  const menu = $('#menu');
  menu.replaceChildren();
  for (const group of groups) {
    if (menu.children.length) menu.append(document.createElement('hr'));
    for (const item of group) {
      const button = document.createElement('button');
      button.setAttribute('role', 'menuitem');
      if (item.danger) button.className = 'danger';
      button.textContent = item.label;
      button.addEventListener('click', () => { hideMenu(); item.run(); });
      menu.append(button);
    }
  }
  if (!menu.children.length) return;
  if (row) row.classList.add('menu-target');
  menu.hidden = false;
  // Open where clicked, but keep it inside the window.
  const { width, height } = menu.getBoundingClientRect();
  menu.style.left = `${Math.max(0, Math.min(x, innerWidth - width - 4))}px`;
  menu.style.top = `${Math.max(0, Math.min(y, innerHeight - height - 4))}px`;
}

function hideMenu() {
  $('#menu').hidden = true;
  for (const row of $('#messages').querySelectorAll('.menu-target')) row.classList.remove('menu-target');
}

function copyText(text) {
  navigator.clipboard.writeText(text).catch((e) => toast(`Couldn't copy: ${e}`));
}

// ---------- Accounts & settings ----------

function openSettings() {
  // Every time, so a release published while the app was open shows up here.
  checkForUpdate();
  $('#settings').hidden = false;
  $('#close-settings').hidden = !state.configured;
  $('#client-secret').placeholder = state.configured ? 'Saved (enter again to change)' : 'GOCSPX-…';
  renderSettingsAccounts();
  if (!state.configured) $('#client-id').focus();
}

function closeSettings() {
  if (!state.configured) return;
  $('#settings').hidden = true;
}

/**
 * Rust sets the window's theme and saves the choice. The window theme is also what the
 * page's `prefers-color-scheme` reports, so the stylesheet's dark mode follows it.
 */
async function setTheme(theme) {
  try {
    await invoke('set_theme', { theme });
    state.theme = theme;
    renderTheme();
  } catch (e) {
    toast(String(e));
  }
}

function renderTheme() {
  for (const button of document.querySelectorAll('#theme button')) {
    button.setAttribute('aria-checked', String(button.dataset.theme === state.theme));
  }
}

function renderSettingsAccounts() {
  $('#settings-add').disabled = !state.configured;
  $('#account-list').replaceChildren(...state.accounts.map((account) => {
    const li = document.createElement('li');
    li.style.setProperty('--acc', colorOf(account));
    li.innerHTML = '<i class="dot"></i><span></span><button class="reconnect" hidden>Reconnect</button><button class="remove">Remove</button>';
    li.querySelector('span').textContent = account;
    const reconnect = li.querySelector('.reconnect');
    reconnect.hidden = !state.expired.has(account);
    reconnect.addEventListener('click', () => signIn(account));
    const button = li.querySelector('.remove');
    button.addEventListener('click', async () => {
      if (!button.classList.contains('confirm')) {
        button.classList.add('confirm');
        button.textContent = 'Click to confirm';
        return;
      }
      try {
        await invoke('remove_account', { email: account });
        state.accounts = state.accounts.filter((a) => a !== account);
        for (const folder of Object.keys(state.lists)) {
          state.lists[folder] = state.lists[folder].filter((m) => m.account !== account);
        }
        if (state.filter === account) state.filter = null;
        if (state.current && state.current.account === account) clearReader();
        render();
      } catch (e) {
        toast(String(e));
      }
    });
    return li;
  }));
}

async function saveClient(e) {
  e.preventDefault();
  const clientId = $('#client-id').value.trim();
  const clientSecret = $('#client-secret').value.trim();
  try {
    await invoke('save_client', { clientId, clientSecret });
    state.configured = true;
    $('#client-secret').value = '';
    $('#client-status').textContent = 'Saved.';
    setTimeout(() => { $('#client-status').textContent = ''; }, 2500);
    openSettings();
  } catch (err) {
    $('#client-status').textContent = String(err);
  }
}

/** Accept the client JSON downloaded from Google Cloud Console. */
function pasteClientJson(e) {
  const text = e.clipboardData.getData('text');
  if (!text.trim().startsWith('{')) return;
  try {
    const json = JSON.parse(text);
    const client = json.installed || json.web || json;
    if (!client.client_id) return;
    e.preventDefault();
    $('#client-id').value = client.client_id;
    $('#client-secret').value = client.client_secret || '';
  } catch { /* not JSON; paste normally */ }
}

function addAccount() {
  return signIn();
}

/**
 * Runs Google's sign-in in the browser: for a new account, or with `reconnect` (an email) to
 * sign in again to an account whose sign-in expired. That account is preselected in Google's
 * chooser, and picking another one there changes nothing.
 */
async function signIn(reconnect = null) {
  if (!state.configured) { openSettings(); return; }
  const seq = ++state.authSeq;
  $('#waiting').hidden = false;
  try {
    const email = reconnect
      ? await invoke('reconnect_account', { email: reconnect })
      : await invoke('add_account');
    if (!state.accounts.includes(email)) state.accounts.push(email);
    state.expired.delete(email);
    render();
    toast(`${reconnect ? 'Reconnected' : 'Added'} ${email}`, 2500);
    if (seq === state.authSeq) $('#settings').hidden = true;
    refresh();
  } catch (e) {
    if (seq === state.authSeq) toast(String(e), 6000);
  } finally {
    if (seq === state.authSeq) $('#waiting').hidden = true;
  }
}

// ---------- Updates ----------

/** Asks GitHub whether a newer release exists. Failures (offline, nothing released yet) stay quiet. */
async function checkForUpdate() {
  try {
    state.update = await invoke('check_update');
  } catch (e) {
    console.warn(e);
    return;
  }
  renderUpdate();
}

function renderUpdate() {
  const button = $('#update');
  button.hidden = !state.update;
  if (state.update && !button.disabled) button.textContent = `Update to ${state.update}`;
  $('#open-settings').classList.toggle('has-update', Boolean(state.update));
  $('#open-settings').title = state.update ? `Version ${state.update} is available` : '';
}

/**
 * Downloads and installs the update. On Windows the app closes and the installer opens it
 * again when it's done; on macOS the app restarts itself.
 */
async function installUpdate() {
  const button = $('#update');
  button.disabled = true;
  button.textContent = 'Updating…';
  try {
    await invoke('install_update');
  } catch (e) {
    toast(String(e), 8000);
    button.disabled = false;
    renderUpdate();
    checkForUpdate();  // installing used up the update that was found; find it again
  }
}

function openExternal(url) {
  invoke('open_external', { url }).catch((e) => toast(String(e)));
}

// ---------- Helpers ----------

function setStatus(text) {
  $('#sync-status').textContent = text;
}

let toastTimer;
/**
 * Shows a message at the bottom. With `action` it gets a button: `{ label, run }`, plus
 * `undo: true` for Undo, which `Z` also presses.
 */
function toast(text, ms = 4000, action = null) {
  $('#toast-text').textContent = text;
  $('#toast-action').hidden = !action;
  $('#toast-action').textContent = action ? action.label : '';
  state.toastAction = action ? action.run : null;
  state.undo = action?.undo ? action.run : null;
  $('#toast').hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(hideToast, ms);
}

function hideToast() {
  $('#toast').hidden = true;
  state.toastAction = null;
  state.undo = null;
}

function senderName(from) {
  const match = /^\s*"?([^"<]*?)"?\s*<([^>]+)>/.exec(from || '');
  if (match) return match[1].trim() || match[2];
  return (from || '').trim() || '(unknown sender)';
}

// Dates are in the app's language, English ("Nov 4"), not the system's, which would mix
// languages. The clock still follows the system: 15:31 or 3:31 PM.
const hourCycle = new Intl.DateTimeFormat(undefined, { hour: 'numeric' }).resolvedOptions().hourCycle;
const timeFormat = new Intl.DateTimeFormat('en-US', { hour: 'numeric', minute: '2-digit', hourCycle });
const dayFormat = new Intl.DateTimeFormat('en-US', { day: 'numeric', month: 'short' });
// Month names rather than 11/4/24, which reads differently in different countries.
const yearFormat = new Intl.DateTimeFormat('en-US', { day: 'numeric', month: 'short', year: 'numeric' });
const longFormat = new Intl.DateTimeFormat('en-US', { dateStyle: 'medium', timeStyle: 'short', hourCycle });

function shortDate(ms) {
  const date = new Date(ms);
  const now = new Date();
  if (date.toDateString() === now.toDateString()) return timeFormat.format(date);
  if (date.getFullYear() === now.getFullYear()) return dayFormat.format(date);
  return yearFormat.format(date);
}

function longDate(ms) {
  return longFormat.format(new Date(ms));
}

function escapeHtml(text) {
  return text.replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);
}

/** Runs on already-escaped text, so matched URLs can't contain raw markup. */
function linkify(escaped) {
  return escaped.replace(/\bhttps?:\/\/[^\s<]+[^\s<.,;:!?)'"\]]/g, (url) => `<a href="${url}">${url}</a>`);
}

init().catch((e) => toast(`Startup failed: ${e}`, 10000));
