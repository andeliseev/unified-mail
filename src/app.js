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
// How long Undo stays available after an action. A sent email waits this long before it goes
// (send_message), plus SEND_GRACE_MS, so an Undo clicked at the last moment still arrives in time.
const UNDO_MS = 8000;
const SEND_GRACE_MS = 1000;
// Gmail sends up to 25 MB of attachments with an email (mime.rs checks it again).
const MAX_ATTACHMENTS = 25 * 1024 * 1024;
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
  /* The frame is as tall as the email (fitHeight): the reader scrolls it, the frame only sideways,
     for an email too wide to shrink. Some emails set overflow on html/body; :root outranks their
     html/body selectors, so this wins even against !important. */
  :root { overflow-x: auto !important; overflow-y: hidden !important; }
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
  sent: { name: 'Sent', empty: 'Nothing sent yet.', icon: 'M10 14L21 3M21 3l-6.5 18a.55.55 0 0 1-1 0L10 14l-7-3.5a.55.55 0 0 1 0-1z' },
  drafts: { name: 'Drafts', empty: 'No drafts.', icon: 'M14 3v4a1 1 0 0 0 1 1h4M17 21H7a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h7l5 5v11a2 2 0 0 1-2 2zM10 18l5-5a1.4 1.4 0 0 0-2-2l-5 5v2z' },
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
    case 'sent': return ['trash'];
    // Drafts are written, sent or discarded (see Writing), not moved.
    case 'drafts': return [];
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
    leave: ['inbox', 'sent', 'all'],
    join: ['trash'],
    run: (m) => invoke('trash_message', ids(m)),
    undo: (m, { wasInInbox, wasSent }) => ({
      leave: ['trash'],
      join: [...(wasInInbox ? ['inbox'] : []), ...(wasSent ? ['sent'] : []), 'all'],
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
  // Rows in `drafts` also say which Gmail draft they are (`draft`).
  lists: { inbox: [], sent: [], drafts: [], all: [], trash: [], search: [] },
  loaded: { inbox: false, sent: false, drafts: false, all: false, trash: false, search: false },  // list filled from cache or a sync
  synced: { inbox: false, sent: false, drafts: false, all: false, trash: false },  // a sync finished this session
  listErrors: {},  // per folder, the accounts its last sync couldn't load: [{ account, message }]
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
  // Messages being written (see newDraft): new ones, each a row at the top of the list, newest
  // first; the one the reader shows (or null); replies, by the key of the email they answer.
  drafts: [],
  composing: null,
  replies: new Map(),
  lastFrom: null,        // the account a message was last sent from: a new one starts there
  sendJob: 0,
  sending: new Set(),    // `account/draft id` of Gmail drafts being sent: the Drafts folder leaves them out
  // Per folder (and the search) and account: `frontier`, the oldest date loaded so far without
  // gaps, and `more`, whether older mail exists. Scrolling pushes the frontier back.
  paging: { inbox: {}, sent: {}, drafts: {}, all: {}, trash: {}, search: {} },
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
  emailLinks: new Map(),  // the open email's links, by address: what its text shows instead, or '' (checkLinks)
  linkCheck: null,        // the address `askLink` asks about
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
  // The sidebar counts the drafts.
  setList('drafts', await invoke('cached_folder', { folder: 'drafts' }));
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
  // macOS: a link clicked in the email, and the link under the pointer (macos.rs).
  await listen('email-link', (e) => openEmailLink(e.payload));
  await listen('email-link-hover', (e) => showLinkAddress(e.payload));
  setInterval(() => { if (!document.hidden) refresh(); }, AUTO_REFRESH_MS);
  window.addEventListener('focus', () => {
    if (Date.now() - state.lastSync > REFRESH_ON_FOCUS_AFTER_MS) refresh();
  });
  // Drafts are saved as soon as the window goes behind another, is hidden or closed, rather
  // than when typing pauses (not the page's own `blur`, which a click into the email fires).
  window.__TAURI__.window.getCurrentWindow().onFocusChanged(({ payload: focused }) => {
    if (!focused) saveAllDrafts();
  });
  document.addEventListener('visibilitychange', () => { if (document.hidden) saveAllDrafts(); });
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
  // macOS the message's own button row (or a new message's), level with the list header and the
  // window buttons.
  const strips = IS_MAC ? [...document.querySelectorAll('.msg-actions')] : [$('#window-bar')];
  // "deep": anywhere inside drags, except on buttons.
  for (const el of [$('.brand'), $('.list-head'), ...strips]) el.dataset.tauriDragRegion = 'deep';
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
  $('#compose-open').title = 'New message (C)';
  $('#compose-open').addEventListener('click', () => openCompose());
  $('#m-reply').addEventListener('click', () => startReply('reply'));
  $('#m-more').addEventListener('click', (e) => showAnswerMenu(e.currentTarget, (kind) => startReply(kind)));
  $('#c-send').title = `Send (${IS_MAC ? '⌘' : 'Ctrl+'}Enter)`;
  $('#c-send').addEventListener('click', () => state.composing && sendDraft(state.composing, $('#compose')));
  $('#c-attach').addEventListener('click', () => state.composing && pickFiles(state.composing, () => {
    renderComposeFiles();
    $('#c-body').focus();
  }));
  $('#c-discard').addEventListener('click', () => state.composing && discardDraft(state.composing));
  $('#c-body').addEventListener('input', () => {
    if (!state.composing) return;
    state.composing.body = $('#c-body').value;
    draftChanged(state.composing);
  });
  $('#compose').addEventListener('keydown', (e) => onEditorKey(e, state.composing, $('#compose')));
  $('#file-picker').addEventListener('change', onFilesPicked);
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
  // "This link may not go where it says" (askLink): a click beside the box cancels, as Cancel does.
  $('#lc-open').addEventListener('click', () => {
    const href = state.linkCheck;
    closeLinkCheck();
    if (href) openExternal(href);
  });
  $('#lc-copy').addEventListener('click', () => {
    if (state.linkCheck) copyText(state.linkCheck.replace(/^mailto:/i, ''));
    closeLinkCheck();
  });
  $('#lc-cancel').addEventListener('click', closeLinkCheck);
  $('#link-check').addEventListener('mousedown', (e) => { if (e.target === $('#link-check')) closeLinkCheck(); });
  $('#update').addEventListener('click', installUpdate);
  for (const button of document.querySelectorAll('#theme button')) {
    button.addEventListener('click', () => setTheme(button.dataset.theme));
  }
  $('#client-form').addEventListener('submit', saveClient);
  $('#client-id').addEventListener('paste', pasteClientJson);

  $('#messages').addEventListener('click', (e) => {
    const row = e.target.closest('li.msg');
    if (!row) return;
    if (row.dataset.draft) {
      openDraft(draftById(row.dataset.draft));
      return;
    }
    if (row.dataset.gmailDraft) {
      openGmailDraft(byKey(row.dataset.gmailDraft));
      return;
    }
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
  // A wider or narrower reader (resizing the window) changes how much an email must shrink, and
  // so how tall it is. Only the width counts: the frame's own height changes #m-body's.
  let readerWidth = 0;
  new ResizeObserver(() => {
    const width = $('#m-body').clientWidth;
    if (width === readerWidth) return;
    readerWidth = width;
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
      state.listErrors[folder] = results[i].errors;
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
    for (const folder of folders) state.listErrors[folder] = [{ account: null, message: String(e) }];
    state.syncing = false;  // first, so an empty list says why rather than "Loading…"
    render();
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
    count: folder === 'inbox' ? inboxUnread : folder === 'drafts' ? draftCount() : 0,
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
  // The Drafts folder: drafts being written here, then Gmail's other drafts.
  const drafts = folder === 'drafts';
  const messages = drafts ? gmailDrafts() : visible();
  // In All Mail and search results, tag what's still in the Inbox (everything else there is
  // archived or sent).
  const inboxKeys = folder === 'all' || folder === 'search'
    ? new Set([...state.lists.inbox.map(keyOf), ...state.inboxExtra.keys()])
    : null;
  const marks = folder === 'search' ? searchPattern(state.search.query) : null;
  const fragment = document.createDocumentFragment();
  // New messages being written stay at the top of every list. The Drafts folder lists answers
  // too, once something is written in them, and narrows them to the account chosen.
  const written = (d) => (d.gmailId || hasContent(d) || d === state.composing) && (!state.filter || d.from === state.filter);
  for (const draft of drafts ? localDrafts().filter(written) : state.drafts) fragment.append(draftRow(draft));
  for (const m of messages) {
    if (drafts) {
      fragment.append(gmailDraftRow(m));
      continue;
    }
    const li = document.createElement('li');
    li.className = 'msg' + (m.unread ? ' unread' : '') + (keyOf(m) === state.selected ? ' selected' : '');
    li.dataset.key = keyOf(m);
    li.style.setProperty('--acc', colorOf(m.account));
    li.innerHTML = `<div class="row1"><span class="from"></span>${m.hasFiles ? CLIP_ICON : ''}<time></time></div>` +
      '<div class="subject"><span class="text"></span></div><div class="snippet"></div>';
    // Sent mail shows whom it went to, as in Gmail.
    if (folder === 'sent') li.querySelector('.from').textContent = `To: ${recipientNames(m.to) || '(no recipients)'}`;
    else setMarkedText(li.querySelector('.from'), senderName(m.from), marks);
    li.querySelector('time').textContent = shortDate(m.date);
    setMarkedText(li.querySelector('.subject .text'), m.subject || '(no subject)', marks);
    if (inboxKeys && inboxKeys.has(keyOf(m))) li.querySelector('.subject').prepend(tag('Inbox'));
    // A reply to it is being written.
    if (state.replies.has(keyOf(m))) li.querySelector('.subject').prepend(tag('Draft', 'draft'));
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

  const empty = !$('#messages').children.length;
  $('#list-empty').hidden = !empty;
  $('#messages').hidden = empty;
  if (empty) {
    const noAccounts = state.accounts.length === 0;
    let text;
    if (noAccounts) text = 'Add your Gmail account to get started.';
    else if (folder === 'search') text = !state.search.done ? 'Searching…' : searchFailure() || `Nothing matches “${state.search.query}”.`;
    else if (state.syncing && !state.synced[folder]) text = 'Loading…';
    else text = listFailure(folder) || FOLDERS[folder].empty;
    $('#list-empty-text').textContent = text;
    $('#empty-add').hidden = !noAccounts;
  }
}

/**
 * Why a folder with nothing to show may still have mail: its last sync couldn't load it, for the
 * account chosen in the sidebar or for any. Said in place of "Inbox zero".
 */
function listFailure(folder) {
  const failed = (state.listErrors[folder] || []).filter((e) => !e.account || !state.filter || e.account === state.filter);
  if (!failed.length) return '';
  const [first] = failed;
  const others = failed.length > 1 ? ` and ${failed.length - 1} more` : '';
  return `Couldn't load ${FOLDERS[folder].name}${first.account ? ` for ${first.account}${others}` : ''}: ${first.message}`;
}

/** Why a search with nothing to show may have missed mail: an account on screen it couldn't search. */
function searchFailure() {
  const failed = [...state.search.failed].filter(([account]) => !state.filter || account === state.filter);
  if (!failed.length) return '';
  const [[account, error]] = failed;
  const others = failed.length > 1 ? ` and ${failed.length - 1} more` : '';
  return `Couldn't search ${account}${others}: ${error}`;
}

function tag(text, kind = '') {
  const span = document.createElement('span');
  span.className = `tag ${kind}`.trim();
  span.textContent = text;
  return span;
}

/** The row of a new message being written: whom it's to, its subject and its start. */
function draftRow(draft) {
  const li = document.createElement('li');
  const open = state.composing === draft || (draft.message && state.current && keyOf(state.current) === keyOf(draft.message));
  li.className = 'msg draft' + (open ? ' selected' : '');
  li.dataset.draft = draft.id;
  li.innerHTML = '<div class="row1"><span class="from"><span class="who"></span></span></div>' +
    '<div class="subject"><span class="text"></span></div><div class="snippet"></div>';
  li.querySelector('.from').prepend(tag('Draft', 'draft'));
  fillDraftRow(li, draft);
  return li;
}

/** Whom a message went to, by name: "Anna Petrova, mark@example.com". */
const recipientNames = (to) => parseAddresses(to || '').map((r) => r.name || r.email).join(', ');

/** A row of the Drafts folder for one of Gmail's drafts: whom it's to, its subject and its start. */
function gmailDraftRow(m) {
  const li = document.createElement('li');
  li.className = 'msg draft';
  li.dataset.gmailDraft = keyOf(m);
  li.style.setProperty('--acc', colorOf(m.account));
  li.title = `Draft in ${m.account}`;
  li.innerHTML = '<div class="row1"><span class="from"><span class="who"></span></span><time></time></div>' +
    '<div class="subject"><span class="text"></span></div><div class="snippet"></div>';
  li.querySelector('.from').prepend(tag('Draft', 'draft'));
  li.querySelector('.who').textContent = recipientNames(m.to) || 'No recipients yet';
  li.querySelector('time').textContent = shortDate(m.date);
  li.querySelector('.subject .text').textContent = m.subject || '(no subject)';
  li.querySelector('.snippet').textContent = m.snippet || '\u00a0';
  return li;
}

function fillDraftRow(li, draft) {
  li.style.setProperty('--acc', colorOf(draft.from));
  li.title = `Draft from ${draft.from}`;
  li.querySelector('.who').textContent = draft.to.map((r) => r.name || r.email).join(', ') || 'No recipients yet';
  li.querySelector('.subject .text').textContent = draft.subject || '(no subject)';
  li.querySelector('.snippet').textContent = draft.body.replace(/\s+/g, ' ').trim() || ' ';
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
  // Answering quotes the email, so it waits for the email to load.
  $('#m-reply').disabled = $('#m-more').disabled = !state.full;
}

// ---------- Search ----------

/** Opens the search field in the list's header: the magnifier, `/`, or Cmd+F (Ctrl+F on Windows). */
function openSearch() {
  if (!state.accounts.length) return;
  if (!state.search) {
    state.search = { query: '', seq: 0, done: false, failed: new Map(), timer: null, scroll: $('#messages').scrollTop };
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
  search.failed = new Map();  // account → why it couldn't be searched
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
      search.failed.set(account, String(error));
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
  $('#search-open').hidden = $('#compose-open').hidden = !state.accounts.length;
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
    const shown = state.filter ? [state.filter] : state.accounts.filter((account) => !state.expired.has(account));
    const failed = shown.length > 0 && shown.every((account) => search.failed.has(account));
    text = count ? `${count}${more ? '+' : ''} ${count === 1 && !more ? 'result' : 'results'} ${where}`
      : failed ? `Couldn't search ${where}` : `No results ${where}`;
  }
  $('#search-scope').textContent = text;
}

// ---------- Reading ----------

/** Opens message `key` in the reader; `given` is its summary when it isn't in the list on screen. */
async function select(key, given = null) {
  const m = given || byKey(key);
  if (!m) return;
  leaveMessage(key);
  const wasComposing = Boolean(state.composing);
  leaveCompose();
  state.selected = key;
  state.current = m;
  state.full = null;
  state.originalColors = false;  // each email starts dark in dark mode
  if (wasComposing) renderList();  // the draft's row isn't the one selected any more (or is gone)
  markRow(key);
  renderActions();

  renderReaderViews();
  $('#m-scroll').scrollTop = 0;
  $('#m-end').replaceChildren();
  $('#m-subject').textContent = m.subject || '(no subject)';
  $('#m-from').textContent = m.from;
  $('#m-date').textContent = longDate(m.date);
  $('#m-to').textContent = '';
  $('#m-replyto').hidden = true;
  $('#m-account').textContent = m.account;
  $('#m-account').style.setProperty('--acc', colorOf(m.account));
  renderAttachments(m, []);
  showBodyMessage('Loading…');

  const seq = ++state.openSeq;
  try {
    const full = await invoke('get_message', { account: m.account, id: m.id });
    if (seq !== state.openSeq) return;
    $('#m-to').textContent = 'to ' + [full.to, full.cc].filter(Boolean).join(', ');
    renderReplyTo(full);
    full.inline = inlinePictures(full);
    renderAttachments(m, (full.attachments || []).filter((a) => !full.inline.includes(a)));
    showBody(full);
    renderActions();
    // An answer opened from Drafts quotes the email once it's here.
    const reply = state.replies.get(key);
    if (reply && !reply.full) reply.full = full;
    renderMessageEnd();
    if (full.inline.length) loadInlinePictures(m, full, seq);
    // Opening marks it read, as in Gmail.
    if ((byKey(key) || m).unread) setUnread(m, false);
  } catch (e) {
    if (seq === state.openSeq) showBodyMessage(`Couldn't load this message: ${e}`);
  }
}

/** Closes the open email. A new message being written stays in the reader. */
function clearReader() {
  leaveMessage();
  state.selected = null;
  state.current = null;
  state.full = null;
  state.openSeq++;
  $('#m-body').replaceChildren();
  $('#m-end').replaceChildren();
  renderReaderViews();
}

/** The reader shows a new message being written, else the open email, else "Select a message". */
function renderReaderViews() {
  const composing = Boolean(state.composing);
  $('#compose').hidden = !composing;
  $('#message').hidden = composing || !state.current;
  $('#reader-empty').hidden = composing || Boolean(state.current);
}

// ---------- Writing: new messages, replies and forwards ----------

const svg = (inner, className = '') =>
  `<svg${className ? ` class="${className}"` : ''} viewBox="0 0 24 24" aria-hidden="true">${inner}</svg>`;
const CROSS = '<path d="M6 6l12 12M18 6L6 18"/>';
const CHEVRON = '<path d="M6 9l6 6 6-6"/>';
const PAPERCLIP = '<path d="M15 7l-6.5 6.5a1.5 1.5 0 0 0 3 3L18 10a3 3 0 0 0-6-6l-6.5 6.5a4.5 4.5 0 0 0 9 9L21 13"/>';
const DOTS = '<circle class="dot" cx="5" cy="12" r="1.3"/><circle class="dot" cx="12" cy="12" r="1.3"/><circle class="dot" cx="19" cy="12" r="1.3"/>';
const TRASH = '<path d="M3 6h18M8 6V4h8v2M6 6l1 14h10l1-14M10 10v6M14 10v6"/>';
const CHECK = '<path d="M5 12l5 5L20 7"/>';
// The ways to answer an email, with their keys.
const KINDS = {
  reply: { label: 'Reply', key: 'R', icon: '<path d="M9 14l-4-4 4-4M5 10h11a4 4 0 0 1 0 8h-1"/>' },
  replyAll: { label: 'Reply all', key: 'A', icon: '<path d="M13 14l-4-4 4-4M8 14l-4-4 4-4M9 10h7a4 4 0 0 1 0 8h-1"/>' },
  forward: { label: 'Forward', key: 'F', icon: '<path d="M15 14l4-4-4-4M19 10H8a4 4 0 0 0 0 8h1"/>' },
};

let draftSeq = 0;
/**
 * A message being written. `kind`: 'new', or how it answers `message` (the email's summary):
 * 'reply', 'replyAll' or 'forward'; `full` is that email, for the quote. A new message can still
 * answer one, without the quote (`thread`: a Gmail draft whose email wasn't found). `from`: the
 * account it goes from. Recipients are `{ name, email }`. `files` were picked on this computer or
 * came with a Gmail draft (`{ name, mimeType, size, data }`, data in base64); `carried` are the
 * answered email's files that go along when forwarding (its attachments, as `get_message` lists
 * them).
 * Saving it to Gmail's Drafts (saveDraft): `gmailId` and `gmailAccount` once it's saved there,
 * `dirty` while it has changes to save, `saving` the last save's promise, `saveState` what the
 * editor says about it, and `closed` once it's sent or discarded, so nothing saves it again.
 */
function newDraft(kind, from, message = null, full = null) {
  return {
    id: String(++draftSeq), kind, from, message, full, thread: null,
    to: [], cc: [], bcc: [], showCc: false, subject: '', body: '', files: [], carried: [], showQuote: false,
    gmailId: null, gmailAccount: null, dirty: false, saving: null, saveTimer: null, saveState: '', saveError: '', closed: false,
  };
}

/** Messages being written here: new ones, then answers. */
const localDrafts = () => [...state.drafts, ...state.replies.values()];
const draftById = (id) => localDrafts().find((d) => d.id === id) || null;
/** A reply with nothing written in it yet: leaving its email drops it. */
const isEmptyDraft = (draft) => !draft.body.trim() && !draft.files.length;
/** A new message with nothing in it at all: leaving it drops it. */
const isBlankDraft = (draft) =>
  isEmptyDraft(draft) && !draft.subject.trim() && !draft.to.length && !draft.cc.length && !draft.bcc.length;
/** Something worth keeping was written: it's saved, and counts as a draft. */
const hasContent = (draft) => (draft.message ? !isEmptyDraft(draft) : !isBlankDraft(draft));
/** A new message goes from the account chosen in the sidebar, else the one last sent from. */
const defaultFrom = () => state.filter || (state.accounts.includes(state.lastFrom) ? state.lastFrom : state.accounts[0]);

/**
 * Gmail's drafts that aren't being written or sent here: the Drafts folder's rows from Gmail.
 * The account chosen in the sidebar narrows them, as in other folders.
 */
function gmailDrafts() {
  const here = new Set(localDrafts().filter((d) => d.gmailId).map((d) => `${d.gmailAccount}/${d.gmailId}`));
  const list = listName() === 'drafts' ? visible() : state.lists.drafts.filter((m) => !state.filter || m.account === state.filter);
  return list.filter((m) => m.draft && !here.has(`${m.account}/${m.draft}`) && !state.sending.has(`${m.account}/${m.draft}`));
}

/** The Drafts folder's count: drafts written here and Gmail's others. */
const draftCount = () =>
  localDrafts().filter((d) => (d.gmailId || hasContent(d)) && (!state.filter || d.from === state.filter)).length + gmailDrafts().length;

/**
 * Leaving the open email: a reply to it with nothing written yet is dropped, else it's saved.
 * One opened from Gmail's Drafts is never dropped: what was written in it may be where the
 * editor doesn't show it, or only in its recipients, and dropping it deletes it from Gmail.
 */
function leaveMessage(nextKey = null) {
  const m = state.current;
  if (!m || keyOf(m) === nextKey) return;
  const draft = state.replies.get(keyOf(m));
  if (!draft) return;
  if (isEmptyDraft(draft) && !draft.fromGmail) {
    dropDraft(draft);
    renderList();
  } else {
    saveDraft(draft);
  }
}

/**
 * Leaving the new message in the reader: it stays in the list and is saved, unless there's
 * nothing in it (and it wasn't opened from Gmail's Drafts, as in `leaveMessage`).
 */
function leaveCompose() {
  const draft = state.composing;
  if (!draft) return;
  state.composing = null;
  if (isBlankDraft(draft) && !draft.fromGmail) dropDraft(draft);
  else saveDraft(draft);
  renderSidebar();
}

/** Drops a draft with nothing in it, here and from Gmail's Drafts. */
function dropDraft(draft) {
  draft.closed = true;
  clearTimeout(draft.saveTimer);
  if (draft.message) {
    if (state.replies.get(keyOf(draft.message)) === draft) state.replies.delete(keyOf(draft.message));
  } else {
    state.drafts = state.drafts.filter((d) => d !== draft);
  }
  deleteGmailDraft(draft).catch((e) => console.warn(e));
}

/** Deletes `draft`'s Gmail draft, once a save on its way is done. */
function deleteGmailDraft(draft) {
  return (draft.saving || Promise.resolve()).then(() => deleteGmailDraftNow(draft));
}

/** Deletes `draft`'s Gmail draft now: from a save itself, which mustn't wait for itself. */
async function deleteGmailDraftNow(draft) {
  if (!draft.gmailId) return;
  const [account, id] = [draft.gmailAccount, draft.gmailId];
  draft.gmailId = null;
  forgetGmailDraft(account, id);
  await invoke('delete_draft', { account, id });
}

/** A Gmail draft sent or deleted leaves the Drafts folder's rows. */
function forgetGmailDraft(account, id) {
  state.lists.drafts = state.lists.drafts.filter((m) => !(m.account === account && m.draft === id));
  renderSidebar();
  if (listName() === 'drafts') renderList();
}

// A draft is saved to its account's Gmail Drafts this long after typing stops. One with more
// than BIG_DRAFT of files waits a minute, or until it's left, since every save sends its files
// again. A failed save is tried again after DRAFT_RETRY_MS.
const DRAFT_SAVE_MS = 2000;
const BIG_DRAFT_SAVE_MS = 60 * 1000;
const BIG_DRAFT = 5 * 1024 * 1024;
const DRAFT_RETRY_MS = 30 * 1000;

/** Something in `draft` changed: its row follows, and it's saved once typing pauses. */
function draftChanged(draft) {
  const row = $('#messages').querySelector(`li[data-draft="${draft.id}"]`);
  if (row) fillDraftRow(row, draft);
  if (draft.closed) return;
  draft.dirty = true;
  clearTimeout(draft.saveTimer);
  draft.saveTimer = setTimeout(() => saveDraft(draft), draftSize(draft) > BIG_DRAFT ? BIG_DRAFT_SAVE_MS : DRAFT_SAVE_MS);
  if (draft.saveState !== 'saving') setSaveState(draft, '');
}

/**
 * Saves `draft` to its account's Gmail Drafts now (`save_draft`): the first time as a new draft,
 * then over it. Saves run one after another, so a draft is never created twice, and each sends
 * what's written when it starts. Nothing is saved before something is written, nor an answer
 * before its email has loaded (it carries the quote). A draft whose From changed leaves the
 * other account's Drafts. Returns this save's promise.
 */
function saveDraft(draft) {
  clearTimeout(draft.saveTimer);
  if (!draft.dirty || draft.closed) return draft.saving || Promise.resolve();
  draft.dirty = false;
  draft.saving = (draft.saving || Promise.resolve()).then(async () => {
    if (draft.closed || (!draft.gmailId && !hasContent(draft))) return;
    if (draft.message && !draft.full) {
      draft.dirty = true;
      return;
    }
    setSaveState(draft, 'saving');
    try {
      if (draft.gmailId && draft.gmailAccount !== draft.from) await deleteGmailDraftNow(draft);
      const payload = { ...outgoing(draft), draftId: draft.gmailId };
      const saved = await invoke('save_draft', { draft: payload });
      draft.gmailId = saved.id;
      // The account it was saved in: From may have changed meanwhile, and the next save moves it.
      draft.gmailAccount = payload.account;
      setSaveState(draft, draft.dirty ? '' : 'saved');
    } catch (e) {
      draft.dirty = true;
      draft.saveError = String(e);
      setSaveState(draft, 'error');
      clearTimeout(draft.saveTimer);
      if (!draft.closed) draft.saveTimer = setTimeout(() => saveDraft(draft), DRAFT_RETRY_MS);
    }
  });
  return draft.saving;
}

/** Saves every draft with changes now: the window went behind another or was hidden. */
function saveAllDrafts() {
  for (const draft of localDrafts()) if (draft.dirty) saveDraft(draft);
}

function setSaveState(draft, saveState) {
  draft.saveState = saveState;
  renderSaveState(draft);
  if (saveState === 'saved') renderSidebar();  // the Drafts count
}

/** What the editor says about saving: "Saving…", "Saved to Gmail", or "Not saved" with why. */
function renderSaveState(draft) {
  const open = state.composing === draft ? $('#c-saved')
    : draft.message && state.current && keyOf(state.current) === keyOf(draft.message) ? $('#m-end .save-state') : null;
  if (!open) return;
  const [text, title] = {
    saving: ['Saving…', ''],
    saved: [draft.message ? 'Saved' : 'Saved to Gmail', `Saved in the Gmail Drafts of ${draft.gmailAccount}`],
    error: ['Not saved', `Couldn't save the draft: ${draft.saveError}`],
  }[draft.saveState] || ['', ''];
  open.className = `save-state ${draft.saveState}`;
  open.title = title;
  open.innerHTML = draft.saveState === 'saved' ? svg(CHECK) : '';
  open.append(text);
}

/** Opens a draft written here: an answer under its email, a new message in the editor. */
function openDraft(draft) {
  if (!draft) return;
  if (draft.message) openReplyDraft(draft);
  else openCompose(draft);
}

/** Shows the email an answer being written belongs to, with the answer under it. */
async function openReplyDraft(draft) {
  const key = keyOf(draft.message);
  if (state.current && keyOf(state.current) === key && state.full) {
    // Its email is already open, so `select` won't give it the email to quote and thread with.
    draft.full ||= state.full;
    renderMessageEnd();
  } else {
    await select(key, byKey(key) || draft.message);
  }
  focusReply(draft);
}

/**
 * Opens one of Gmail's drafts (`open_draft`), to go on writing it: an answer under its email
 * when that's found in its conversation, anything else in the editor.
 */
async function openGmailDraft(m) {
  if (!m || !m.draft) return;
  const here = (id) => localDrafts().find((d) => d.gmailId === id && d.gmailAccount === m.account);
  if (here(m.draft)) {
    openDraft(here(m.draft));
    return;
  }
  setStatus('Opening the draft…');
  const [seq, composing] = [state.openSeq, state.composing];
  try {
    const opened = await invoke('open_draft', { account: m.account, id: m.draft });
    // Something else was opened meanwhile: the draft doesn't take the reader (and the typing) from it.
    const still = seq === state.openSeq && state.composing === composing;
    if (here(opened.id)) {
      if (still) openDraft(here(opened.id));
      return;
    }
    const draft = draftFromGmail(m.account, opened);
    if (draft.message) state.replies.set(keyOf(draft.message), draft);
    else state.drafts.unshift(draft);
    renderList();
    if (still) openDraft(draft);
  } catch (e) {
    toast(`Couldn't open the draft: ${e}`, 6000);
  } finally {
    setStatus('');
  }
}

/**
 * A draft as Gmail keeps it (`open_draft`), to write here. An answer goes under its email, as
 * long as no other answer to that email is being written; its quote is made again from the email
 * when it's saved, so the draft's own quote and the pictures in it are left out.
 */
function draftFromGmail(account, o) {
  const answered = o.answered && !state.replies.has(keyOf(o.answered)) ? o.answered : null;
  const to = parseAddresses(o.to);
  const cc = parseAddresses(o.cc);
  const forward = /^(fwd?|fw):/i.test(o.subject || '');
  const kind = !answered ? 'new' : forward ? 'forward' : to.length + cc.length > 1 ? 'replyAll' : 'reply';
  const draft = newDraft(kind, account, answered);
  Object.assign(draft, {
    to, cc, bcc: parseAddresses(o.bcc), subject: o.subject || '', body: typedText(o),
    gmailId: o.id, gmailAccount: account, saveState: 'saved', fromGmail: true,
  });
  draft.showCc = Boolean(draft.cc.length || draft.bcc.length);
  const html = o.html ? decodeBody(o.html) : '';
  const quoted = (f) => answered && f.contentId && html.includes(`cid:${f.contentId}`);
  draft.files = o.files.filter((f) => !quoted(f)).map(({ name, mimeType, size, data }) => ({ name, mimeType, size, data }));
  if (!answered && o.inReplyTo) draft.thread = { threadId: o.threadId, messageId: o.inReplyTo, references: o.references };
  return draft;
}

/**
 * What was written in a Gmail draft: its text without the email it quotes or forwards. Those
 * start at a line like "On …, Anna <a@b.c> wrote:" (which may wrap) or "Forwarded message".
 * In an answer, lines without ">" after that line were written under the quote or between its
 * lines, as some mail apps answer: they're kept too, after the text above it.
 */
function typedText(o) {
  if (o.text) {
    let text = decodeBody(o.text).replace(/\r\n?/g, '\n');
    const quote = o.inReplyTo && text.match(/^(On .*(\n.*)?wrote:[ \t]*$|-{5,} ?Forwarded message ?-{5,})/m);
    if (quote) {
      const after = quote[1].startsWith('On ') ? text.slice(quote.index + quote[0].length) : '';
      const between = after.split('\n').filter((line) => line.trim() && !line.startsWith('>')).join('\n');
      text = [text.slice(0, quote.index).replace(/\s+$/, ''), between].filter(Boolean).join('\n\n');
    }
    return text.replace(/\s+$/, '');
  }
  if (!o.html) return '';
  const doc = new DOMParser().parseFromString(decodeBody(o.html), 'text/html');
  for (const quote of doc.querySelectorAll('.gmail_quote')) quote.remove();
  return htmlToText(doc.body.innerHTML);
}

/**
 * Writes a new message in the reader (the pencil, or `C`), or brings back `draft`. Without one,
 * a message already being written comes back, so there's one row to find it by.
 */
function openCompose(draft = null) {
  if (!state.accounts.length) return;
  draft ||= state.composing || state.drafts[0] || null;
  if (state.composing !== draft) leaveCompose();
  if (!draft) {
    draft = newDraft('new', defaultFrom());
    state.drafts.unshift(draft);
  }
  leaveMessage();
  state.selected = null;
  state.current = null;
  state.full = null;
  state.openSeq++;
  $('#m-body').replaceChildren();
  $('#m-end').replaceChildren();
  state.composing = draft;
  renderCompose();
  renderSaveState(draft);
  renderReaderViews();
  renderList();
  renderActions();
  (draft.to.length ? $('#c-body') : $('#c-fields .rcpts input')).focus();
}

/** The new message's fields: From, To (Cc, Bcc), Subject and its files; then its text. */
function renderCompose() {
  const draft = state.composing;
  const from = document.createElement('div');
  from.className = 'field';
  from.innerHTML = '<span class="fk">From</span>';
  from.append(fromPicker(draft));
  const showCc = linkButton('Cc Bcc', () => {
    draft.showCc = true;
    renderCompose();
    $('#c-fields input[aria-label="Cc"]').focus();
  });
  const rows = [from, recipientRow(draft, 'to', { label: 'To', placeholder: 'Name or email', tail: draft.showCc ? null : showCc })];
  if (draft.showCc) rows.push(recipientRow(draft, 'cc', { label: 'Cc' }), recipientRow(draft, 'bcc', { label: 'Bcc' }));
  const subject = document.createElement('div');
  subject.className = 'field';
  subject.innerHTML = '<span class="fk">Subject</span><input class="subject" aria-label="Subject">';
  const input = subject.querySelector('input');
  input.value = draft.subject;
  input.addEventListener('input', () => {
    draft.subject = input.value;
    draftChanged(draft);
  });
  rows.push(subject, composeFiles(draft));
  $('#c-fields').replaceChildren(...rows);
  $('#c-body').value = draft.body;
}

function composeFiles(draft) {
  // The ✕ that was clicked is replaced: the text gets the focus back, so typing goes on there.
  const row = filesRow(draft, () => {
    $('#c-fields .draft-files').replaceWith(composeFiles(draft));
    $('#c-body').focus();
  });
  row.classList.add('field');
  return row;
}

function renderComposeFiles() {
  if (state.composing) $('#c-fields .draft-files').replaceWith(composeFiles(state.composing));
}


/** From in a new message: the account, and a menu of the others. */
function fromPicker(draft) {
  const button = document.createElement('button');
  button.type = 'button';
  button.className = 'from-pick';
  button.title = 'Send from';
  const draw = () => {
    const label = document.createElement('span');
    label.className = 'label';
    label.textContent = draft.from;
    button.replaceChildren(accountDot(colorOf(draft.from)), label);
    button.insertAdjacentHTML('beforeend', svg(CHEVRON));
  };
  button.addEventListener('click', () => {
    const box = button.getBoundingClientRect();
    showMenu([state.accounts.map((account) => ({
      label: account,
      color: colorOf(account),
      run: () => {
        draft.from = account;
        draw();
        draftChanged(draft);
      },
    }))], box.left, box.bottom + 4);
  });
  draw();
  return button;
}

function accountDot(color) {
  const dot = document.createElement('span');
  dot.className = 'acc-dot';
  dot.style.setProperty('--acc', color);
  return dot;
}

function linkButton(label, onClick) {
  const button = document.createElement('button');
  button.type = 'button';
  button.className = 'link-btn';
  button.textContent = label;
  button.addEventListener('click', onClick);
  return button;
}

/**
 * A row of recipients (`draft[field]`: To, Cc or Bcc): a chip each, then a field to type more.
 * Enter, a comma or leaving the field turns what's typed into chips; Backspace in the empty field
 * removes the last one. Pasting a list adds each address. `lead` goes first in place of a label
 * (a reply's kind of answer), `tail` last (Cc Bcc). The field's `commit()` adds what's typed.
 */
function recipientRow(draft, field, { label = '', placeholder = '', lead = null, tail = null }) {
  const row = document.createElement('div');
  row.className = 'field rcpts';
  if (label) {
    const name = document.createElement('span');
    name.className = 'fk';
    name.textContent = label;
    row.append(name);
  }
  if (lead) row.append(lead);
  const input = document.createElement('input');
  input.setAttribute('aria-label', label || 'To');
  input.spellcheck = false;
  input.autocomplete = 'off';
  const changed = () => {
    draw();
    draftChanged(draft);
  };
  const draw = () => {
    for (const chip of row.querySelectorAll('.rcpt')) chip.remove();
    draft[field].forEach((r, i) => input.before(recipientChip(r, () => {
      draft[field].splice(i, 1);
      changed();
      input.focus();
    })));
    input.placeholder = draft[field].length ? '' : placeholder;
  };
  input.commit = () => {
    const added = parseAddresses(input.value).filter((r) => !draft[field].some((x) => sameEmail(x, r)));
    input.value = '';
    if (!added.length) return;
    draft[field].push(...added);
    changed();
  };
  input.addEventListener('keydown', (e) => {
    if (e.isComposing) return;
    if ((e.key === 'Enter' || e.key === ',' || e.key === ';') && input.value.trim()) {
      e.preventDefault();
      input.commit();
    } else if (e.key === 'Backspace' && !input.value && input.selectionStart === 0 && draft[field].length) {
      e.preventDefault();
      draft[field].pop();
      changed();
    }
  });
  input.addEventListener('blur', () => input.commit());
  input.addEventListener('paste', (e) => {
    const text = e.clipboardData.getData('text');
    if (!/[,;\n]/.test(text)) return;
    e.preventDefault();
    input.value += text;
    input.commit();
  });
  // A click in the row's empty space goes to the field.
  row.addEventListener('mousedown', (e) => {
    if (e.target !== row) return;
    e.preventDefault();
    input.focus();
  });
  row.append(input);
  if (tail) row.append(tail);
  draw();
  return row;
}

/** A recipient's chip: the name (the address under the mouse), red if it isn't an address. */
function recipientChip(r, remove) {
  const chip = document.createElement('span');
  const ok = isAddress(r.email);
  chip.className = 'rcpt' + (ok ? '' : ' bad');
  chip.title = ok ? r.email : `“${r.email}” isn't an email address`;
  chip.innerHTML = `<span class="who"></span><button type="button" tabindex="-1" aria-label="Remove">${svg(CROSS)}</button>`;
  chip.querySelector('.who').textContent = r.name || r.email;
  chip.querySelector('button').addEventListener('click', remove);
  return chip;
}

/**
 * Addresses as people type or paste them, and as email headers list them:
 * `Anna <anna@example.com>, "Petrova, Anna" <a@b.c>; mark@example.com`.
 */
function parseAddresses(text) {
  const found = [];
  const add = (piece) => {
    piece = piece.trim();
    if (!piece) return;
    const named = /^(.*?)\s*<([^<>]*)>$/.exec(piece);
    if (named) {
      const name = named[1].trim().replace(/^"(.*)"$/, '$1').replace(/\\(.)/g, '$1').trim();
      found.push({ name, email: named[2].trim() });
    } else {
      found.push({ name: '', email: piece.replace(/^mailto:/i, '') });
    }
  };
  let piece = '';
  let quoted = false;
  let angle = false;
  for (const c of text) {
    if (c === '"' && !angle) quoted = !quoted;
    else if (c === '<' && !quoted) angle = true;
    else if (c === '>' && !quoted) angle = false;
    if (/[,;\n]/.test(c) && !quoted && !angle) {
      add(piece);
      piece = '';
    } else {
      piece += c;
    }
  }
  add(piece);
  return found;
}

/** Whether it looks like an address Gmail can send to; `check_address` in mime.rs agrees. */
function isAddress(email) {
  if (email.length > 254 || !/^[\x21-\x7e]+$/.test(email) || /[<>()[\]\\,;:"]/.test(email)) return false;
  const [local, domain, extra] = email.split('@');
  return extra === undefined && Boolean(local) && Boolean(domain) && domain.includes('.') &&
    !domain.startsWith('.') && !domain.endsWith('.') && !domain.includes('..');
}

const sameEmail = (a, b) => a.email.toLowerCase() === b.email.toLowerCase();
const uniqueAddresses = (list) => list.filter((r, i) => list.findIndex((x) => sameEmail(x, r)) === i);

/**
 * The files going with a message, each with a cross to take it off: the answered email's when
 * forwarding, then those picked. `redraw` runs after one is taken off.
 */
function filesRow(draft, redraw) {
  const row = document.createElement('div');
  row.className = 'attachments draft-files';
  const add = (file, remove) => {
    const kind = attachmentKind({ name: file.name, mimeType: file.mimeType || '' });
    const chip = document.createElement('span');
    chip.className = `draft-file ${kind}`;
    chip.title = cleanFileName(file.name);
    chip.innerHTML = `<svg class="kind" viewBox="0 0 24 24" aria-hidden="true">${ATTACHMENT_ICONS[kind]}</svg>` +
      `<span class="name"></span><span class="size"></span><button type="button" title="Remove" aria-label="Remove">${svg(CROSS)}</button>`;
    fillFileName(chip.querySelector('.name'), file.name);
    const risk = riskTag(file.name);
    if (risk) chip.querySelector('.name').after(risk);
    chip.querySelector('.size').textContent = formatSize(file.size);
    chip.querySelector('button').addEventListener('click', () => {
      remove();
      draftChanged(draft);
      redraw();
    });
    row.append(chip);
  };
  for (const a of draft.carried) add(a, () => { draft.carried = draft.carried.filter((x) => x !== a); });
  for (const f of draft.files) add(f, () => { draft.files = draft.files.filter((x) => x !== f); });
  row.hidden = !row.children.length;
  return row;
}

let filesFor = null;  // the draft the file picker is open for, and what to do once files are added
/** Opens the system's file picker to attach files to `draft`; `done` runs once they're added. */
function pickFiles(draft, done) {
  filesFor = { draft, done };
  const picker = $('#file-picker');
  picker.value = '';
  picker.click();
}

/** Reads the picked files into the draft, as long as they fit in what Gmail sends. */
async function onFilesPicked() {
  const picked = [...$('#file-picker').files];
  const target = filesFor;
  filesFor = null;
  if (!target || !picked.length) return;
  const { draft, done } = target;
  let total = draftSize(draft);
  for (const file of picked) {
    if (total + file.size > MAX_ATTACHMENTS) {
      toast(`${file.name} is too big to attach: Gmail sends up to 25 MB of files with an email.`, 8000);
      continue;
    }
    try {
      draft.files.push({ name: file.name, mimeType: file.type, size: file.size, data: await readBase64(file) });
      total += file.size;
    } catch (e) {
      toast(`Couldn't attach ${file.name}: ${e}`, 6000);
    }
  }
  draftChanged(draft);
  done();
}

function readBase64(file) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result).replace(/^data:[^,]*,/, ''));
    reader.onerror = () => reject(reader.error ? reader.error.message : 'it could not be read');
    reader.readAsDataURL(file);
  });
}

/** Bytes of files going with the draft: picked, carried along, and the quoted email's pictures. */
function draftSize(draft) {
  const inline = draft.full && draft.kind !== 'new' ? draft.full.inline || [] : [];
  return [...draft.files, ...draft.carried, ...inline].reduce((sum, f) => sum + (f.size || 0), 0);
}

/** Keys in a message being written: Cmd+Enter (Ctrl+Enter) sends it, Esc leaves it. */
function onEditorKey(e, draft, container) {
  if (!draft || e.isComposing) return;
  if (e.key === 'Enter' && (IS_MAC ? e.metaKey : e.ctrlKey)) {
    e.preventDefault();
    sendDraft(draft, container);
  } else if (e.key === 'Escape' && $('#menu').hidden) {
    e.preventDefault();
    if (draft.message) {
      e.target.blur();
      return;
    }
    // A new message closes, and waits in the list.
    document.activeElement.blur();
    leaveCompose();
    renderReaderViews();
    renderList();
  }
}

/** Starts answering the open email (or goes back to the answer already started) as `kind`. */
function startReply(kind) {
  const m = openMessage();
  if (!m || !state.full || state.composing) return;
  const key = keyOf(m);
  let draft = state.replies.get(key);
  if (!draft) {
    draft = newDraft(kind, m.account, { ...m }, state.full);
    setKind(draft, kind);
    state.replies.set(key, draft);
    renderList();
  } else if (draft.kind !== kind) {
    setKind(draft, kind);
  }
  renderMessageEnd();
  focusReply(draft);
}

/**
 * Makes `draft` a reply, a reply to all or a forward of its email, keeping its text: whom it
 * goes to, its subject, and for a forward the email's files. Answering one's own email goes to
 * whom it went to, as in Gmail. The account it's from is never among the recipients.
 */
function setKind(draft, kind) {
  const { full } = draft;
  const mine = (r) => r.email.toLowerCase() === draft.from.toLowerCase();
  draft.kind = kind;
  draft.subject = subjectFor(kind, full.subject);
  draft.bcc = [];
  if (kind === 'forward') {
    draft.to = [];
    draft.cc = [];
    draft.carried = (full.attachments || []).filter((a) => !(full.inline || []).includes(a));
  } else {
    const fromMe = parseAddresses(full.from).some(mine);
    const sender = parseAddresses(full.replyTo || full.from);
    const toList = parseAddresses(full.to);
    let to = fromMe ? toList : sender;
    let cc = [];
    if (kind === 'replyAll') {
      if (!fromMe) to = to.concat(toList);
      cc = parseAddresses(full.cc);
    }
    to = uniqueAddresses(to).filter((r) => !mine(r));
    cc = uniqueAddresses(cc).filter((r) => !mine(r) && !to.some((x) => sameEmail(x, r)));
    // An email to oneself alone gets its answer there too.
    draft.to = to.length ? to : sender;
    draft.cc = cc;
    draft.carried = [];
  }
  draft.showCc = draft.cc.length > 0;
}

/** `Re: Lunch` or `Fwd: Lunch`, without adding a second one. */
function subjectFor(kind, subject) {
  const s = subject || '';
  if (kind === 'forward') return /^(fwd?|fw):/i.test(s) ? s : `Fwd: ${s}`;
  return /^re:/i.test(s) ? s : `Re: ${s}`;
}

/** Under the open email: Reply, Reply all and Forward, or the answer being written. */
function renderMessageEnd() {
  const m = openMessage();
  const end = $('#m-end');
  if (!m || !state.full) {
    end.replaceChildren();
    return;
  }
  const draft = state.replies.get(keyOf(m));
  end.replaceChildren(draft ? replyBox(draft) : answerButtons());
  const text = end.querySelector('textarea');
  if (text) growText(text);
  if (draft) renderSaveState(draft);
}

function answerButtons() {
  const row = document.createElement('div');
  row.className = 'answer-buttons';
  for (const [kind, k] of Object.entries(KINDS)) {
    const button = document.createElement('button');
    button.title = `${k.label} (${k.key})`;
    button.innerHTML = svg(k.icon);
    button.append(k.label);
    button.addEventListener('click', () => startReply(kind));
    row.append(button);
  }
  return row;
}

/**
 * An answer being written, under its email: the kind of answer and whom it goes to, its files,
 * its text (which grows as it's written), the quoted email when shown (···), and Send, attach,
 * the account it goes from, and discard.
 */
function replyBox(draft) {
  const box = document.createElement('div');
  box.className = 'reply-box';
  const redraw = (focus = '') => {
    renderMessageEnd();
    if (focus) $(`#m-end ${focus}`)?.focus();
  };
  const kind = document.createElement('button');
  kind.type = 'button';
  kind.className = 'kind-btn';
  kind.title = 'Reply, Reply all or Forward';
  kind.setAttribute('aria-label', `${KINDS[draft.kind].label}: change`);
  kind.innerHTML = svg(KINDS[draft.kind].icon) + svg(CHEVRON, 'chev');
  kind.addEventListener('click', () => showAnswerMenu(kind, (k) => {
    setKind(draft, k);
    draftChanged(draft);
    redraw(k === 'forward' && !draft.to.length ? '.rcpts input' : 'textarea');
  }));
  const showCc = linkButton('Cc Bcc', () => {
    draft.showCc = true;
    redraw('input[aria-label="Cc"]');
  });
  box.append(recipientRow(draft, 'to', { placeholder: 'Add recipients', lead: kind, tail: draft.showCc ? null : showCc }));
  if (draft.showCc) box.append(recipientRow(draft, 'cc', { label: 'Cc' }), recipientRow(draft, 'bcc', { label: 'Bcc' }));
  // Redrawing replaces the button that was clicked, so the text gets the focus back: on the
  // page, letters typed next would be shortcuts (E archives, R/A/F change the answer).
  box.append(filesRow(draft, () => redraw('textarea')));

  const text = document.createElement('textarea');
  text.setAttribute('aria-label', 'Message');
  text.placeholder = draft.kind === 'forward' ? 'Add a message' : 'Write your reply';
  text.value = draft.body;
  text.addEventListener('input', () => {
    draft.body = text.value;
    growText(text);
    draftChanged(draft);
  });
  box.append(text);
  if (draft.showQuote) {
    const quoted = document.createElement('div');
    quoted.className = 'quote';
    quoted.textContent = quotePreview(draft);
    box.append(quoted);
  }

  const foot = document.createElement('div');
  foot.className = 'reply-foot';
  const send = document.createElement('button');
  send.className = 'primary';
  send.textContent = 'Send';
  send.title = `Send (${IS_MAC ? '⌘' : 'Ctrl+'}Enter)`;
  send.addEventListener('click', () => sendDraft(draft, box));
  const attach = iconButton(PAPERCLIP, 'Attach files', () => pickFiles(draft, () => redraw('textarea')));
  const quote = iconButton(DOTS, draft.showQuote ? 'Hide the quoted email' : 'Show the quoted email', () => {
    draft.showQuote = !draft.showQuote;
    redraw('textarea');
  });
  quote.classList.toggle('on', draft.showQuote);
  const saved = document.createElement('span');
  saved.className = 'save-state';
  const from = document.createElement('span');
  from.className = 'reply-from';
  from.title = 'Answers go from the account the email came to';
  const label = document.createElement('span');
  label.className = 'label';
  label.textContent = draft.from;
  from.append('from', accountDot(colorOf(draft.from)), label);
  const discard = iconButton(TRASH, 'Discard', () => discardDraft(draft));
  foot.append(send, attach, quote, saved, from, discard);
  box.append(foot);
  box.addEventListener('keydown', (e) => onEditorKey(e, draft, box));
  return box;
}

function iconButton(icon, title, onClick) {
  const button = document.createElement('button');
  button.type = 'button';
  button.className = 'icon-btn';
  button.title = title;
  button.setAttribute('aria-label', title);
  button.innerHTML = svg(icon);
  button.addEventListener('click', onClick);
  return button;
}

/** A menu of Reply, Reply all and Forward under `anchor`. */
function showAnswerMenu(anchor, pick) {
  const box = anchor.getBoundingClientRect();
  showMenu([Object.entries(KINDS).map(([kind, k]) => ({ label: k.label, hint: k.key, run: () => pick(kind) }))], box.left, box.bottom + 4);
}

/** Scrolls the reader to the answer being written and puts the cursor in it. */
function focusReply(draft) {
  const box = $('#m-end .reply-box');
  if (!box) return;
  const field = draft.kind === 'forward' && !draft.to.length ? box.querySelector('.rcpts input') : box.querySelector('textarea');
  field.focus({ preventScroll: true });
  box.scrollIntoView({ block: 'nearest' });
}

/**
 * The reply's text is as tall as what's written; the reader keeps its place meanwhile, and
 * follows the cursor down while writing at the end.
 */
function growText(text) {
  const scroller = $('#m-scroll');
  const top = scroller.scrollTop;
  text.style.height = 'auto';
  text.style.height = `${text.scrollHeight}px`;
  scroller.scrollTop = top;
  if (document.activeElement === text && text.selectionEnd === text.value.length) {
    const below = text.getBoundingClientRect().bottom - scroller.getBoundingClientRect().bottom;
    if (below > 0) scroller.scrollTop += below + 12;
  }
}

/** What's wrong with sending `draft` as it is, for people; null if nothing. */
function draftProblem(draft) {
  const everyone = [...draft.to, ...draft.cc, ...draft.bcc];
  if (!everyone.length) return 'Add someone to send it to.';
  const wrong = everyone.find((r) => !isAddress(r.email));
  if (wrong) return `“${wrong.email}” isn't an email address. Check it and send again.`;
  if (!state.accounts.includes(draft.from)) return `${draft.from} isn't in Unified Mail any more. Pick another account to send from.`;
  if (state.expired.has(draft.from)) return `The sign-in has expired for ${draft.from}. Click Reconnect above the message list, then send again.`;
  return null;
}

/**
 * Sends `draft` (Send, or Cmd+Enter in it): what's typed in its recipient fields counts. It
 * leaves the screen at once, and the toast's Undo can bring it back while `send_message` waits
 * out UNDO_MS. If sending fails, it comes back with the reason.
 */
async function sendDraft(draft, container) {
  for (const input of container.querySelectorAll('.rcpts input')) input.commit();
  const problem = draftProblem(draft);
  if (problem) {
    toast(problem, 6000);
    return;
  }
  closeDraft(draft);
  const job = ++state.sendJob;
  // Undo before `send_message` has started just brings it back; after, it asks `cancel_send`.
  const flow = { undone: false, started: false };
  let gmail = '';  // `account/id` of the Gmail draft it goes as, left out of Drafts meanwhile
  toast(`Sent from ${draft.from}`, UNDO_MS, {
    label: 'Undo',
    undo: true,
    run: async () => {
      if (flow.started && !(await invoke('cancel_send', { job }).catch(() => false))) {
        toast('Too late to undo: it has been sent.', 4000);
        return;
      }
      flow.undone = true;
      state.sending.delete(gmail);
      reopenDraft(draft);
    },
  });
  try {
    // A save on its way finishes first, so the draft it made is the one that's sent.
    await draft.saving;
    if (flow.undone) return;
    // Its From changed since it was saved: that account's draft goes, and this one is new.
    if (draft.gmailId && draft.gmailAccount !== draft.from) {
      await deleteGmailDraft(draft).catch(() => {});
      if (flow.undone) return;
    }
    if (draft.gmailId) {
      gmail = `${draft.gmailAccount}/${draft.gmailId}`;
      state.sending.add(gmail);
      renderSidebar();
      if (listName() === 'drafts') renderList();
    }
    flow.started = true;
    const payload = { ...outgoing(draft), draftId: draft.gmailId };
    const sent = await invoke('send_message', { job, delayMs: UNDO_MS + SEND_GRACE_MS, draft: payload });
    if (!sent) return;  // Undo
    if (draft.gmailId) forgetGmailDraft(draft.gmailAccount, draft.gmailId);
    draft.gmailId = null;
    state.lastFrom = draft.from;
    refresh();
  } catch (e) {
    reopenDraft(draft);
    toast(`Couldn't send the message: ${e}`, 10000);
  } finally {
    state.sending.delete(gmail);
  }
}

/** Discards `draft`, deleting it from Gmail's Drafts; the toast's Undo saves it there again. */
function discardDraft(draft) {
  closeDraft(draft);
  const gone = deleteGmailDraft(draft).catch((e) => toast(`Couldn't delete the draft from Gmail: ${e}`, 6000));
  toast('Draft discarded', UNDO_MS, {
    label: 'Undo',
    undo: true,
    run: async () => {
      await gone;
      reopenDraft(draft);
    },
  });
}

/** Takes `draft` off the screen (its row, its email's Draft tag, the editor); it isn't saved again. */
function closeDraft(draft) {
  draft.closed = true;
  clearTimeout(draft.saveTimer);
  if (draft.message) {
    const key = keyOf(draft.message);
    if (state.replies.get(key) === draft) state.replies.delete(key);
    if (state.current && keyOf(state.current) === key) renderMessageEnd();
  } else {
    state.drafts = state.drafts.filter((d) => d !== draft);
    if (state.composing === draft) {
      state.composing = null;
      renderReaderViews();
    }
  }
  renderList();
  renderSidebar();
}

/**
 * Brings `draft` back after Undo, or after sending failed: in the reader if it can show it.
 * It's saved again, in case Gmail's copy is older.
 */
function reopenDraft(draft) {
  draft.closed = false;
  if (!draft.message) {
    if (!state.drafts.includes(draft)) state.drafts.unshift(draft);
    if (!state.accounts.includes(draft.from)) draft.from = defaultFrom();
    openCompose(draft);
    draftChanged(draft);
    return;
  }
  const key = keyOf(draft.message);
  state.replies.set(key, draft);
  draftChanged(draft);
  renderList();
  if (state.current && keyOf(state.current) === key) {
    renderMessageEnd();
    focusReply(draft);
  } else if (byKey(key)) {
    select(key);
  }
}

/**
 * A removed account's new messages go from another one, as new drafts there; answers to its
 * mail are dropped. Its own Gmail drafts stay in its Gmail.
 */
function forgetDraftsOf(account) {
  for (const draft of state.drafts) {
    if (draft.from === account) draft.from = state.accounts[0];
    if (draft.gmailAccount === account) draft.gmailId = draft.gmailAccount = null;
  }
  state.drafts = state.drafts.filter((d) => d.from);
  if (state.composing && !state.drafts.includes(state.composing)) {
    state.composing = null;
    renderReaderViews();
  } else if (state.composing) {
    renderCompose();
  }
  for (const [key, draft] of state.replies) {
    if (draft.from !== account) continue;
    draft.closed = true;
    clearTimeout(draft.saveTimer);
    state.replies.delete(key);
  }
}

/** The email a reply quotes or a forward carries: the line(s) introducing it, and its text. */
function quoteOf(draft) {
  const { full } = draft;
  const intro = draft.kind === 'forward'
    ? ['---------- Forwarded message ---------', `From: ${full.from}`, `Date: ${longDate(full.date)}`,
      `Subject: ${full.subject}`, `To: ${full.to}`, ...(full.cc ? [`Cc: ${full.cc}`] : [])]
    : [`On ${longDate(full.date)}, ${full.from} wrote:`];
  const text = full.text ? decodeBody(full.text) : full.html ? htmlToText(decodeBody(full.html)) : '';
  return { intro, text: text.trim() };
}

function quotePreview(draft) {
  const { intro, text } = quoteOf(draft);
  return `${intro.join('\n')}\n\n${text}`;
}

/**
 * What `send_message` gets for `draft`. A new message is plain text. An answer also has HTML,
 * like Gmail's: the text, then the email answered in its own formatting, quoted (a reply) or
 * under a "Forwarded message" header, with the pictures it shows in place and, forwarding, its
 * files. The plain text quotes the email as text, `>` before each line.
 */
function outgoing(draft) {
  const typed = draft.body.replace(/\s+$/, '');
  const payload = {
    account: draft.from,
    to: draft.to,
    cc: draft.cc,
    bcc: draft.bcc,
    subject: draft.subject,
    text: typed,
    html: null,
    answers: null,
    files: draft.files.map(({ name, mimeType, data }) => ({ name, mimeType, data })),
    parts: [],
  };
  if (!draft.message || !draft.full) {
    if (draft.thread) payload.answers = { id: '', threadId: draft.thread.threadId, messageId: draft.thread.messageId || '', references: draft.thread.references || '' };
    return payload;
  }
  const { full, message } = draft;
  const forward = draft.kind === 'forward';
  const { intro, text } = quoteOf(draft);
  payload.answers = { id: message.id, threadId: full.threadId || message.threadId, messageId: full.messageId || '', references: full.references || '' };
  const before = typed ? `${typed}\n\n` : '';
  payload.text = forward
    ? `${before}${intro.join('\n')}\n\n${text}`
    : `${before}${intro[0]}\n${text.split('\n').map((line) => (line ? `> ${line}` : '>')).join('\n')}`;
  const body = emailHtml(full);
  const typedHtml = typed ? `<div dir="auto">${linkify(escapeHtml(typed)).replace(/\n/g, '<br>')}</div><br>` : '';
  const quoted = forward
    ? `<div class="gmail_quote"><div dir="ltr" class="gmail_attr">${intro.map(escapeHtml).join('<br>')}<br></div><br>${body}</div>`
    : `<div class="gmail_quote"><div dir="ltr" class="gmail_attr">${escapeHtml(intro[0])}<br></div>` +
      `<blockquote class="gmail_quote" style="margin:0 0 0 .8ex;border-left:1px solid #ccc;padding-left:1ex">${body}</blockquote></div>`;
  payload.html = `<!doctype html><html><head><meta charset="utf-8"></head><body>${typedHtml}${quoted}</body></html>`;
  payload.parts = [
    ...draft.carried.map((a) => ({ part: a.part })),
    ...(full.inline || []).map((a) => ({ part: a.part, contentId: a.contentId })),
  ];
  return payload;
}

/**
 * The answered email's HTML body as an answer quotes it. Its `<style>` rules are left out: in
 * the recipient's mail app they'd also style, or hide, what's written above the quote. So is
 * fixed or absolute positioning, which could cover it. Parsing doesn't load anything or run
 * anything. Plain text is escaped, keeping its line breaks.
 */
function emailHtml(full) {
  if (!full.html) {
    const text = full.text ? decodeBody(full.text) : '';
    return `<div>${linkify(escapeHtml(text)).replace(/\n/g, '<br>')}</div>`;
  }
  const doc = new DOMParser().parseFromString(decodeBody(full.html), 'text/html');
  for (const el of doc.querySelectorAll('script, style, base, meta, title, link')) el.remove();
  for (const el of doc.body.querySelectorAll('[style]')) {
    if (/^(fixed|absolute|sticky)$/i.test(el.style.position)) el.style.position = 'static';
  }
  return doc.body.innerHTML;
}

/** An HTML email's text, with a line break wherever a block (paragraph, row...) ends. */
function htmlToText(html) {
  const doc = new DOMParser().parseFromString(html, 'text/html');
  const out = [];
  const walk = (node) => {
    for (const child of node.childNodes) {
      if (child.nodeType === Node.TEXT_NODE) {
        out.push(child.data.replace(/\s+/g, ' '));
      } else if (child.nodeType === Node.ELEMENT_NODE && !/^(SCRIPT|STYLE|HEAD|TITLE)$/.test(child.tagName)) {
        if (child.tagName === 'BR') {
          out.push('\n');
          continue;
        }
        const block = /^(P|DIV|TR|LI|H[1-6]|BLOCKQUOTE|TABLE|UL|OL|PRE|HR|SECTION|ARTICLE|HEADER|FOOTER)$/.test(child.tagName);
        if (block) out.push('\n');
        walk(child);
        if (block) out.push('\n');
        else if (/^T[DH]$/.test(child.tagName)) out.push(' ');
      }
    }
  };
  walk(doc.body);
  return out.join('').replace(/[ \t ]+\n/g, '\n').replace(/\n[ \t ]+/g, '\n').replace(/\n{3,}/g, '\n\n').trim();
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
  chip.title = `Save ${cleanFileName(a.name)} to Downloads`;
  chip.innerHTML = `<svg class="kind" viewBox="0 0 24 24" aria-hidden="true">${ATTACHMENT_ICONS[kind]}</svg>` +
    `<span class="name"></span><span class="save-label"><span class="size idle"></span>${SAVING_LABELS}</span>${SAVE_ICON}${CANCEL_ICON}`;
  fillFileName(chip.querySelector('.name'), a.name);
  const risk = riskTag(a.name);
  if (risk) chip.querySelector('.name').after(risk);
  chip.querySelector('.size').textContent = formatSize(a.size);
  chip.addEventListener('click', () => saveAttachments(m, [a], chip));
  return chip;
}

const IMAGE_EXTENSIONS = ['jpg', 'jpeg', 'png', 'gif', 'webp', 'heic', 'heif', 'avif', 'bmp', 'tif', 'tiff', 'ico'];
const ARCHIVE_EXTENSIONS = ['zip', 'rar', '7z', 'gz', 'tgz', 'tar', 'xz', 'bz2'];
const DOCUMENT_EXTENSIONS = ['doc', 'docx', 'xls', 'xlsx', 'ppt', 'pptx', 'odt', 'ods', 'odp', 'rtf', 'txt', 'csv', 'pages', 'numbers', 'key'];

/**
 * Picks the icon and its color by the file's extension, which is what decides how the system
 * opens it, whatever type the sender gave it: `invoice.pdf.html` sent as a PDF is no PDF. A name
 * without an extension goes by that type.
 */
function attachmentKind(a) {
  const extension = extensionOf(a.name);
  if (extension) {
    if (IMAGE_EXTENSIONS.includes(extension)) return 'image';
    if (extension === 'pdf') return 'pdf';
    if (ARCHIVE_EXTENSIONS.includes(extension)) return 'archive';
    if (DOCUMENT_EXTENSIONS.includes(extension)) return 'document';
    return 'file';
  }
  const type = a.mimeType || '';
  if (type.startsWith('image/') && type !== 'image/svg+xml') return 'image';
  if (type === 'application/pdf') return 'pdf';
  if (/zip|rar|7z|tar|compressed/.test(type)) return 'archive';
  if (/^text\/plain|word|excel|spreadsheet|presentation|powerpoint|opendocument|rtf/.test(type)) return 'document';
  return 'file';
}

/**
 * Characters that show nothing (zero-width spaces and joiners, soft hyphens, fillers, the blank
 * Braille pattern) or turn the text around, which a sender can put in a file name to push its
 * real extension out of sight: `Invoice.pdf⠀⠀⠀…⠀.html`.
 */
const INVISIBLE_CHARACTERS = /[­͏؜ᅟᅠ឴឵᠋-᠏​-‏‪-‮⁠-⁯⠀ㅤ︀-️﻿ﾠ]/g;

/** A file name as it's shown: without invisible characters, and blank runs as one space. */
function cleanFileName(name) {
  return name.replace(INVISIBLE_CHARACTERS, '').replace(/\s+/g, ' ').trim() || 'attachment';
}

/** A file name's extension, lowercase, or '' (`cleanFileName` first, so blanks can't hide it). */
function extensionOf(name) {
  const match = /\.([a-z0-9]{1,10})$/i.exec(cleanFileName(name));
  return match ? match[1].toLowerCase() : '';
}

/**
 * Fills a chip's `.name` with a file name whose extension always shows: the rest is cut short
 * (with "…") when the chip is too narrow, the extension isn't.
 */
function fillFileName(span, name) {
  const clean = cleanFileName(name);
  const extension = extensionOf(clean);
  const cut = extension ? clean.length - extension.length - 1 : clean.length;
  const base = document.createElement('span');
  base.className = 'base';
  base.textContent = clean.slice(0, cut);
  const end = document.createElement('span');
  end.className = 'ext';
  end.textContent = clean.slice(cut);
  span.replaceChildren(base, end);
}

const OPENS_IN_BROWSER = ['htm', 'html', 'xht', 'xhtml', 'shtml', 'mht', 'mhtml', 'svg', 'svgz'];
const RUNS_PROGRAMS = [
  'exe', 'msi', 'msix', 'msixbundle', 'appx', 'appxbundle', 'bat', 'cmd', 'com', 'scr', 'pif', 'cpl', 'hta', 'js', 'jse',
  'vbs', 'vbe', 'wsf', 'wsh', 'ps1', 'psm1', 'jar', 'lnk', 'url', 'reg', 'inf', 'command', 'sh', 'app', 'pkg', 'dmg',
  'iso', 'img', 'vhd', 'vhdx', 'one',
];
const HAS_MACROS = ['docm', 'dotm', 'xlsm', 'xltm', 'xlam', 'pptm', 'potm', 'ppam', 'ppsm', 'sldm'];

/** For a file the system opens in a web browser, or that can run code: its tag, else ''. */
function attachmentRisk(name) {
  const extension = extensionOf(name);
  if (OPENS_IN_BROWSER.includes(extension)) return 'Opens in your browser';
  if (RUNS_PROGRAMS.includes(extension)) return 'Runs programs';
  if (HAS_MACROS.includes(extension)) return 'Can run macros';
  return '';
}

/** The tag `attachmentRisk` gives a file, as a chip's part, or null. */
function riskTag(name) {
  const risk = attachmentRisk(name);
  if (!risk) return null;
  const tag = document.createElement('span');
  tag.className = 'risk';
  tag.textContent = risk;
  return tag;
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

/**
 * Under the sender, where a reply goes when the email asks for replies at another address
 * (Reply-To), as Reply does: in grey at the sender's own site, in orange anywhere else, where a
 * phishing email collects the answers its sender's name invites.
 */
function renderReplyTo(full) {
  const line = $('#m-replyto');
  const sender = parseAddresses(full.from || '')[0];
  const same = (r) => sender && r.email.toLowerCase() === sender.email.toLowerCase();
  const others = parseAddresses(full.replyTo || '').filter((r) => r.email && !same(r));
  line.hidden = !others.length;
  if (!others.length) return;
  const domain = (email) => email.split('@').pop();
  const elsewhere = !sender || others.some((r) => !sameSite(domain(r.email), domain(sender.email)));
  const addresses = others.map((r) => r.email).join(', ');
  line.textContent = elsewhere ? `Replies go to ${addresses}, not the sender` : `reply to ${addresses}`;
  line.classList.toggle('warn', elsewhere);
}

// ---------- Links in the email ----------

/**
 * The open email's links, by address, each with what its text shows instead when that's
 * another site or address than where it goes: "www.paypal.com" on a link to paypa1-secure.com.
 * Opening one of those asks first (`openEmailLink`). Senders' own tooltips on links go, so only
 * the line at the bottom of the reader (`showLinkAddress`) says where a link goes.
 */
function checkLinks(doc) {
  const links = new Map();
  for (const link of doc.querySelectorAll('a[href], area[href]')) {
    link.removeAttribute('title');
    let href;
    try { href = new URL(link.href).href; } catch { continue; }
    if (!/^(https?|mailto):/i.test(href)) continue;
    const shown = shownElsewhere(link.textContent, href);
    if (shown || !links.has(href)) links.set(href, shown || '');
  }
  return links;
}

/**
 * What a link's text shows, when it reads as a site ("paypal.com", "https://www.paypal.com/x")
 * or an email address, and that's not where the link goes. Text in words ("Sign in") shows
 * nothing to compare. A site counts as the same when one is the other or a part of it
 * (`email.paypal.com` and `paypal.com`).
 */
function shownElsewhere(text, href) {
  const shown = text.trim().replace(/^<(.*)>$/, '$1');
  if (!shown || /\s/.test(shown)) return '';
  const url = new URL(href);
  const mailto = url.protocol === 'mailto:';
  const address = /^(?:mailto:)?([^@\s/]+@[^@\s/]+\.[^@\s/.]{2,})$/i.exec(shown);
  if (address) {
    const to = mailto ? decodeURIComponent(url.pathname).toLowerCase() : '';
    return to === address[1].toLowerCase() ? '' : address[1];
  }
  const site = /^(?:https?:\/\/)?([^\s/?#:@]+\.[^\s/?#:@.]{2,})(?::\d+)?(?:[/?#]\S*)?$/i.exec(shown);
  if (!site) return '';
  let host;
  try { host = new URL(`http://${site[1]}`).hostname; } catch { return ''; }  // Unicode names as their xn-- form
  return !mailto && sameSite(host, url.hostname) ? '' : site[1];
}

/** Whether two hosts are one site: the same, or one inside the other, `www.` aside. */
function sameSite(a, b) {
  const [x, y] = [a, b].map((host) => host.toLowerCase().replace(/^www\./, ''));
  return Boolean(x && y) && (x === y || x.endsWith(`.${y}`) || y.endsWith(`.${x}`));
}

/** A link's address with its site (or a mailto's address) in bold, as nodes for textContent-safe display. */
function addressParts(href) {
  const url = new URL(href);
  const bold = document.createElement('b');
  if (url.protocol === 'mailto:') {
    bold.textContent = decodeURIComponent(url.pathname);
    return ['mailto:', bold];
  }
  bold.textContent = url.host;
  return [`${url.protocol}//`, bold, `${url.pathname === '/' && !url.search && !url.hash ? '' : url.pathname}${url.search}${url.hash}`];
}

/**
 * The line at the bottom of the reader saying where the link under the pointer goes, as a
 * browser's does. `url` is null off a link; links that aren't the open email's show nothing.
 */
function showLinkAddress(url) {
  const line = $('#link-address');
  let href = null;
  try { href = url ? new URL(url).href : null; } catch {}
  if (!href || !state.emailLinks.has(href)) {
    line.hidden = true;
    return;
  }
  if (line.dataset.href !== href) {
    line.replaceChildren(...addressParts(href));
    line.dataset.href = href;
  }
  line.hidden = false;
}

/** Opens a link from the email in the browser, or asks first when its text shows another site (`checkLinks`). */
function openEmailLink(url) {
  let href;
  try { href = new URL(url).href; } catch { return; }
  const shown = state.emailLinks.get(href);
  if (shown) askLink(href, shown);
  else openExternal(href);
}

/** "This link may not go where it says": the site it shows, where it goes, and Open anyway. */
function askLink(href, shown) {
  const url = new URL(href);
  const goes = url.protocol === 'mailto:' ? decodeURIComponent(url.pathname) : url.hostname;
  $('#lc-why').textContent = `It shows ${shown} but goes to ${goes}.`;
  $('#lc-address').replaceChildren(...addressParts(href));
  state.linkCheck = href;
  hideMenu();
  $('#link-check').hidden = false;
  $('#lc-cancel').focus();
}

function closeLinkCheck() {
  $('#link-check').hidden = true;
  state.linkCheck = null;
}

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
  state.emailLinks = new Map();
  showLinkAddress(null);
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
    state.emailLinks = checkLinks(doc);
    // Images can widen the layout as they arrive. Their `load` events don't bubble, so capture them.
    doc.addEventListener('load', () => scheduleFit(frame), true);
    doc.addEventListener('click', (e) => {
      const link = e.target.closest && e.target.closest('a[href], area[href]');
      if (!link) return;
      e.preventDefault();
      if (!link.getAttribute('href').startsWith('#')) openEmailLink(link.href);
    });
    // Where the link under the pointer goes. On macOS this handler doesn't run, and macos.rs
    // passes the link on instead (`email-link-hover`).
    doc.addEventListener('mouseover', (e) => {
      const link = e.target.closest && e.target.closest('a[href], area[href]');
      showLinkAddress(link ? link.href : null);
    });
    // On macOS keys come from macos.rs instead (onEmailKey), so they never act twice.
    if (!IS_MAC) doc.addEventListener('keydown', (e) => onKey(e, true));
    doc.addEventListener('contextmenu', (e) => onContextMenu(e, frame));
    doc.addEventListener('mousedown', hideMenu);
  });
  // This frame is in the page, so leaving it reaches the page on macOS too.
  frame.addEventListener('mouseleave', () => showLinkAddress(null));
  // Once everything has loaded (web fonts too), fit one last time.
  frame.addEventListener('load', () => {
    scheduleFit(frame);
    frame.loaded = true;
  });
  $('#m-body').replaceChildren(frame);
  watchHeight(frame);
}

/**
 * Keeps the frame as tall as the email while its pictures arrive: on macOS their `load` events
 * don't reach this page, and an email whose pictures have no set size grows as each one comes.
 * Checks every HEIGHT_CHECK_MS until a second after the frame's `load`, or HEIGHT_WATCH_MS.
 */
const HEIGHT_CHECK_MS = 250;
const HEIGHT_WATCH_MS = 30 * 1000;
function watchHeight(frame) {
  const start = Date.now();
  let loadedAt = 0;
  const check = () => {
    if (!frame.isConnected) return;
    fitHeight(frame, false);
    if (frame.loaded && !loadedAt) loadedAt = Date.now();
    if ((loadedAt && Date.now() - loadedAt > 1000) || Date.now() - start > HEIGHT_WATCH_MS) return;
    setTimeout(check, HEIGHT_CHECK_MS);
  };
  setTimeout(check, HEIGHT_CHECK_MS);
}

/**
 * Makes the frame as tall as the email, so the reader scrolls the email with its header and the
 * reply, and the frame scrolls only sideways. `exact`: also shrink it, for a reader that got
 * wider. The email's height can't be read in a frame taller than it (`scrollHeight` is then the
 * frame's), so the frame is squeezed to nothing to measure, and set again before anything is
 * drawn; the reader keeps its scroll position. Otherwise it only grows, which needs no squeeze.
 */
function fitHeight(frame, exact = true) {
  const doc = frame.contentDocument;
  if (!doc || doc.URL !== 'about:srcdoc' || !doc.documentElement) return;
  const root = doc.documentElement;
  if (!exact) {
    if (root.scrollHeight > root.clientHeight) frame.style.height = `${frame.offsetHeight + root.scrollHeight - root.clientHeight}px`;
    return;
  }
  const scroller = $('#m-scroll');
  const top = scroller.scrollTop;
  frame.style.height = '0px';
  const height = root.scrollHeight;
  frame.style.height = `${height}px`;
  // A sideways scroll bar, on an email too wide to shrink, takes room at the bottom.
  if (root.scrollHeight > root.clientHeight) frame.style.height = `${height + root.scrollHeight - root.clientHeight}px`;
  scroller.scrollTop = top;
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
  // Shrinking it, or a wider reader, makes it shorter.
  fitHeight(frame);
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
  // The lists it's in now: a failed move puts it back in these only (Delete leaves the Inbox,
  // Sent and All Mail, but an archived message was never in the Inbox).
  const has = (list) => list.some((x) => keyOf(x) === key);
  const held = move.leave.filter((folder) => has(state.lists[folder])
    || (folder === 'inbox' && state.inboxExtra.has(key))
    || (folder === 'all' && has(state.lists.search)));
  const entry = { message: m, leave: move.leave, join: move.join };
  state.pending.set(key, entry);
  shift(m, move.leave, move.join);
  render();
  try {
    return await run();
  } catch (e) {
    shift(m, move.join, held);
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
  if (listName() === 'drafts') return;
  const list = visible();
  if (!list.length) return;
  const index = list.findIndex((m) => keyOf(m) === state.selected);
  const nextIndex = index < 0 ? 0 : Math.min(list.length - 1, Math.max(0, index + delta));
  if (nextIndex !== index) select(keyOf(list[nextIndex]));
}

/**
 * Scrolls the open message by about a screen, or `amount` px; `direction` is 1 (down) or -1 (up).
 * The email's frame doesn't scroll (fitHeight), and Chromium doesn't pass its keys' scrolling on
 * to the reader, so keys in the email scroll the reader here. `native`: the key also went to
 * WebKit (macOS, in the email), which may scroll the reader itself; then this only scrolls if
 * that didn't, so it never scrolls twice.
 */
const LINE_SCROLL = 40;
function scrollMessage(direction, amount = null, native = false) {
  const scroller = $('#m-scroll');
  if (!state.current) return;
  const by = direction * (amount ?? scroller.clientHeight * 0.9);
  if (!native) {
    scroller.scrollBy({ top: by });
    return;
  }
  const top = scroller.scrollTop;
  setTimeout(() => { if (scroller.scrollTop === top) scroller.scrollBy({ top: by }); }, 80);
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
  // While the link check asks, keys are its own: Esc cancels, Tab and Enter work its buttons.
  if (!$('#link-check').hidden) {
    if (e.key === 'Escape') { e.preventDefault(); closeLinkCheck(); }
    return;
  }
  // F5 and Ctrl+R reload the page in WebView2 (Windows), which loses Undo and what isn't saved
  // yet; Ctrl+R is Reply in Outlook. The app has nothing to reload.
  if (e.key === 'F5' || ((e.ctrlKey || e.metaKey) && !e.altKey && shortcutKey(e).toLowerCase() === 'r')) {
    e.preventDefault();
    return;
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
  // Typing goes to the text fields, and a button of the message being written (Send, Attach…)
  // keeps the focus there too.
  if (e.target.closest && e.target.closest('input, textarea, .reply-box, #compose')) return;
  const key = shortcutKey(e);
  // A key held down repeats: fine for moving and scrolling, not for archiving a run of messages.
  if (e.repeat && !['j', 'k', 'ArrowDown', 'ArrowUp', ' ', 'PageDown', 'PageUp'].includes(key)) return;
  // macOS: a key in the email went to WebKit as well (onEmailKey).
  const native = inMessage && IS_MAC;
  switch (key) {
    case 'j': move(1); break;
    case 'k': move(-1); break;
    case 'ArrowDown': case 'ArrowUp':
      // In the email the arrows scroll it; elsewhere they go through the list.
      if (inMessage) scrollMessage(e.key === 'ArrowDown' ? 1 : -1, LINE_SCROLL, native);
      else move(e.key === 'ArrowDown' ? 1 : -1);
      break;
    case ' ': case 'PageDown': case 'PageUp':
      scrollMessage(e.key === 'PageUp' || (e.key === ' ' && e.shiftKey) ? -1 : 1, null, native);
      break;
    case 'Home': case 'End':
      if (!inMessage) return;
      scrollMessage(e.key === 'End' ? 1 : -1, $('#m-scroll').scrollHeight, native);
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
    case 'c': openCompose(); break;
    case 'r': startReply('reply'); break;
    case 'a': startReply('replyAll'); break;
    case 'f': startReply('forward'); break;
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
  const draft = row && draftById(row.dataset.draft);
  if (draft) {
    groups.push([
      { label: 'Open draft', run: () => openDraft(draft) },
      { label: 'Discard draft', danger: true, run: () => discardDraft(draft) },
    ]);
  }
  const gmailDraft = row && row.dataset.gmailDraft && byKey(row.dataset.gmailDraft);
  if (gmailDraft) groups.push([{ label: 'Open draft', run: () => openGmailDraft(gmailDraft) }]);
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
      { label: 'Open link', run: () => openEmailLink(link.href) },
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

/**
 * `groups`: lists of `{ label, run, danger }`, shown with lines between them. An item can also
 * have an account's `color` (a dot before it) and a `hint` after it, such as its key.
 */
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
      if (item.color) button.append(accountDot(item.color));
      button.append(item.label);
      if (item.hint) {
        const hint = document.createElement('span');
        hint.className = 'hint';
        hint.textContent = item.hint;
        button.append(hint);
      }
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
        forgetDraftsOf(account);
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
