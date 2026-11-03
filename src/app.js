'use strict';

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const PALETTE = ['#2f6df6', '#d93025', '#188038', '#e37400', '#9334e6', '#00897b', '#c2185b', '#5f6368'];
// Push (IMAP IDLE) triggers syncs instantly; polling is only a safety net.
const AUTO_REFRESH_MS = 5 * 60 * 1000;
// Gmail often sends several push lines per change; sync once for the burst.
const PUSH_DEBOUNCE_MS = 300;
const REFRESH_ON_FOCUS_AFTER_MS = 30 * 1000;
// How long Undo stays available after an action.
const UNDO_MS = 8000;
const FRAME_CSS = `
  html { background: #fff; color: #202124; }
  body { margin: 0; padding: 18px 24px 28px; font: 14px/1.5 -apple-system, "Segoe UI", system-ui, sans-serif; word-wrap: break-word; }
  img { max-width: 100%; height: auto; }
  pre.plain { white-space: pre-wrap; font: inherit; margin: 0; }
  a { color: #1a5fd6; }
  /* Some emails set overflow: hidden on html/body, which would make them unscrollable.
     :root outranks the emails' html/body selectors, so this wins even against !important. */
  :root { overflow: auto !important; }
  :root > body { overflow: visible !important; }
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
 */
const inInbox = (m) => state.lists.inbox.some((x) => keyOf(x) === keyOf(m));

/** Actions offered on an open message. */
function actionsFor(m) {
  switch (state.folder) {
    case 'inbox': return ['archive', 'trash'];
    case 'all': return inInbox(m) ? ['archive', 'trash'] : ['unarchive', 'trash'];
    default: return ['restore'];
  }
}

/**
 * For each action: the folder lists the message leaves and joins (applied right away,
 * before Gmail answers), the Gmail call, and how to undo it given that call's result.
 */
const ACTIONS = {
  archive: {
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
  accounts: [],
  folder: 'inbox',       // folder on screen
  lists: { inbox: [], all: [], trash: [] },  // each folder's messages, all accounts, newest first
  loaded: { inbox: false, all: false, trash: false },  // list filled from cache or a sync
  synced: { inbox: false, all: false, trash: false },  // a sync finished this session
  filter: null,          // account email, or null for all accounts
  selected: null,        // key of the highlighted row
  current: null,         // message shown in the reader
  pending: new Map(),    // key → move Gmail hasn't confirmed yet; re-applied to sync results
  undo: null,            // what the toast's Undo button does, while it shows
  syncing: false,
  syncQueued: false,
  lastSync: 0,
  openSeq: 0,
  authSeq: 0,
};

const $ = (sel) => document.querySelector(sel);
const keyOf = (m) => `${m.account}/${m.id}`;
const colorOf = (account) => PALETTE[Math.max(0, state.accounts.indexOf(account)) % PALETTE.length];
const visible = () => {
  const list = state.lists[state.folder];
  return state.filter ? list.filter((m) => m.account === state.filter) : list;
};
const byKey = (key) => state.lists[state.folder].find((m) => keyOf(m) === key);

// ---------- Startup ----------

async function init() {
  bindEvents();
  const setup = await invoke('get_setup');
  state.builtInClient = setup.builtInClient;
  state.configured = setup.builtInClient || Boolean(setup.clientId);
  state.accounts = setup.accounts;
  $('#client-section').hidden = setup.builtInClient;
  $('#client-id').value = setup.clientId || '';
  setList('inbox', await invoke('cached_folder', { folder: 'inbox' }));
  render();

  if (!state.configured) openSettings();
  if (state.accounts.length) refresh();

  let pushTimer;
  await listen('mail-changed', () => {
    clearTimeout(pushTimer);
    pushTimer = setTimeout(refresh, PUSH_DEBOUNCE_MS);
  });
  setInterval(() => { if (!document.hidden) refresh(); }, AUTO_REFRESH_MS);
  window.addEventListener('focus', () => {
    if (Date.now() - state.lastSync > REFRESH_ON_FOCUS_AFTER_MS) refresh();
  });
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
  $('#restore').addEventListener('click', () => act(state.folder === 'trash' ? 'restore' : 'unarchive'));
  $('#toast-action').addEventListener('click', runUndo);
  $('#client-form').addEventListener('submit', saveClient);
  $('#client-id').addEventListener('paste', pasteClientJson);

  $('#messages').addEventListener('click', (e) => {
    const row = e.target.closest('li.msg');
    if (row) select(row.dataset.key);
  });
  document.querySelectorAll('a[data-href]').forEach((a) =>
    a.addEventListener('click', (e) => { e.preventDefault(); openExternal(a.dataset.href); }));
  document.addEventListener('keydown', onKey);
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
      setList(folder, results[i].messages);
      state.synced[folder] = true;
    });
    state.lastSync = Date.now();
    render();
    setStatus('');
    // Each folder reports an account's sign-in problem; show it once, and never over Undo.
    const errors = new Set(results.flatMap((r) => r.errors.map((e) => `${e.account}: ${e.message}`)));
    if (errors.size && !state.undo) toast([...errors].join('\n'), 8000);
  } catch (e) {
    setStatus('');
    toast(String(e));
  } finally {
    state.syncing = false;
    $('#refresh').classList.remove('spin');
    if (state.syncQueued) { state.syncQueued = false; refresh(); }
  }
}

/** Stores a folder's messages, re-applying moves Gmail hasn't confirmed yet. */
function setList(folder, messages) {
  let list = messages;
  for (const [key, move] of state.pending) {
    if (move.leave.includes(folder)) list = list.filter((m) => keyOf(m) !== key);
    if (move.join.includes(folder) && !list.some((m) => keyOf(m) === key)) list = withMessage(list, move.message);
  }
  state.lists[folder] = list;
  state.loaded[folder] = true;
}

function withMessage(list, m) {
  return [...list, m].sort((a, b) => b.date - a.date);
}

async function openFolder(folder) {
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
  // An open message's buttons depend on the lists (e.g. Archive vs Move to Inbox in All Mail).
  renderActions();
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
      active: state.filter === account,
      onClick: () => setFilter(account),
    })),
  );
  const title = FOLDERS[state.folder].name + (state.filter ? ` · ${state.filter}` : '');
  $('#list-title').textContent = title;
  $('#list-title').title = title;
  renderSettingsAccounts();
}

/** A sidebar entry: a folder (`icon`), an account (`color`), or "All accounts" (neither). */
function navItem({ label, icon, color, count = 0, active, onClick }) {
  const button = document.createElement('button');
  button.className = 'nav-item' + (active ? ' active' : '') + (icon || color ? '' : ' all');
  button.innerHTML = (icon ? `<svg viewBox="0 0 24 24" aria-hidden="true"><path d="${icon}"/></svg>` : '<span class="dot"></span>') +
    '<span class="label"></span><span class="count"></span>';
  if (color) button.style.setProperty('--acc', color);
  button.querySelector('.label').textContent = label;
  button.querySelector('.count').textContent = count || '';
  button.title = label;
  button.addEventListener('click', onClick);
  return button;
}

function setFilter(account) {
  state.filter = account;
  render();
}

function renderList() {
  const messages = visible();
  // In All Mail, tag what's still in the Inbox (everything else there is archived or sent).
  const inboxKeys = state.folder === 'all' ? new Set(state.lists.inbox.map(keyOf)) : null;
  const fragment = document.createDocumentFragment();
  for (const m of messages) {
    const li = document.createElement('li');
    li.className = 'msg' + (m.unread ? ' unread' : '') + (keyOf(m) === state.selected ? ' selected' : '');
    li.dataset.key = keyOf(m);
    li.style.setProperty('--acc', colorOf(m.account));
    li.innerHTML = '<div class="row1"><span class="from"></span><time></time></div><div class="subject"><span class="text"></span></div><div class="snippet"></div>';
    li.querySelector('.from').textContent = senderName(m.from);
    li.querySelector('time').textContent = shortDate(m.date);
    li.querySelector('.subject .text').textContent = m.subject || '(no subject)';
    if (inboxKeys && inboxKeys.has(keyOf(m))) {
      const tag = document.createElement('span');
      tag.className = 'tag';
      tag.textContent = 'Inbox';
      li.querySelector('.subject').prepend(tag);
    }
    li.querySelector('.snippet').textContent = m.snippet;
    li.title = m.account;
    fragment.append(li);
  }
  $('#messages').replaceChildren(fragment);

  const empty = messages.length === 0;
  $('#list-empty').hidden = !empty;
  $('#messages').hidden = empty;
  if (empty) {
    const noAccounts = state.accounts.length === 0;
    $('#list-empty-text').textContent = noAccounts
      ? 'Add your Gmail account to get started.'
      : state.syncing && !state.synced[state.folder] ? 'Loading…' : FOLDERS[state.folder].empty;
    $('#empty-add').hidden = !noAccounts;
  }
}

function markRow(key) {
  for (const li of $('#messages').children) {
    li.classList.toggle('selected', li.dataset.key === key);
  }
  const row = $('#messages').querySelector('li.selected');
  if (row) row.scrollIntoView({ block: 'nearest' });
}

function renderActions() {
  const allowed = state.current ? actionsFor(state.current) : [];
  $('#archive').hidden = !allowed.includes('archive');
  $('#trash').hidden = !allowed.includes('trash');
  $('#restore').hidden = !allowed.includes('restore') && !allowed.includes('unarchive');
}

// ---------- Reading ----------

async function select(key) {
  const m = byKey(key);
  if (!m) return;
  state.selected = key;
  state.current = m;
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
  showBodyMessage('Loading…');

  const seq = ++state.openSeq;
  try {
    const full = await invoke('get_message', { account: m.account, id: m.id });
    if (seq !== state.openSeq) return;
    $('#m-to').textContent = 'to ' + [full.to, full.cc].filter(Boolean).join(', ');
    showBody(full);
    if (m.unread) {
      // The same message can be in several folder lists (Inbox and All Mail).
      for (const list of Object.values(state.lists)) {
        for (const x of list) if (keyOf(x) === key) x.unread = false;
      }
      const row = $('#messages').querySelector(`li[data-key="${CSS.escape(key)}"]`);
      if (row) row.classList.remove('unread');
      renderSidebar();
    }
  } catch (e) {
    if (seq === state.openSeq) showBodyMessage(`Couldn't load this message: ${e}`);
  }
}

function clearReader() {
  state.selected = null;
  state.current = null;
  state.openSeq++;
  $('#message').hidden = true;
  $('#reader-empty').hidden = false;
  $('#m-body').replaceChildren();
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

  const frame = document.createElement('iframe');
  // allow-same-origin (without allow-scripts) only lets this page attach the link handler.
  frame.setAttribute('sandbox', 'allow-same-origin');
  frame.setAttribute('referrerpolicy', 'no-referrer');
  frame.srcdoc = `<!doctype html><html><head><meta charset="utf-8"><base target="_blank">` +
    `<style>${FRAME_CSS}</style></head><body>${html}</body></html>`;
  frame.addEventListener('load', () => {
    const doc = frame.contentDocument;
    if (!doc) return;
    doc.addEventListener('click', (e) => {
      const link = e.target.closest && e.target.closest('a[href]');
      if (!link) return;
      e.preventDefault();
      if (!link.getAttribute('href').startsWith('#')) openExternal(link.href);
    });
    doc.addEventListener('keydown', (e) => onKey(e, true));
  });
  $('#m-body').replaceChildren(frame);
}

/** Gmail body parts are base64url bytes in the part's charset. */
function decodeBody(body) {
  const binary = atob(body.data.replace(/-/g, '+').replace(/_/g, '/'));
  const bytes = Uint8Array.from(binary, (c) => c.charCodeAt(0));
  let decoder;
  try { decoder = new TextDecoder(body.charset || 'utf-8'); } catch { decoder = new TextDecoder('utf-8'); }
  return decoder.decode(bytes);
}

// ---------- Actions ----------

/**
 * Runs an action on the open message and offers Undo. If the message left the folder on
 * screen, the next one opens; otherwise (e.g. Archive in All Mail) it stays open.
 */
function act(kind) {
  const m = state.current;
  if (!m || !actionsFor(m).includes(kind)) return;
  const action = ACTIONS[kind];
  const list = visible();
  const index = list.findIndex((x) => keyOf(x) === keyOf(m));
  const next = index >= 0 ? list[index + 1] || list[index - 1] : null;

  const done = perform(m, action, () => action.run(m));
  if (!byKey(keyOf(m))) {
    if (next) select(keyOf(next)); else clearReader();
  }
  done.catch((e) => toast(`Couldn't ${action.verb} the message: ${e}`, 6000));

  toast(action.done, UNDO_MS, {
    label: 'Undo',
    run: async () => {
      let result;
      try { result = await done; } catch { return; }  // it failed and was already put back
      const undo = action.undo(m, result);
      try {
        await perform(m, undo, undo.run);
        // Reopen the message if it's back in the folder on screen.
        if (byKey(keyOf(m))) select(keyOf(m));
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

/** Takes the message out of the `leave` lists and adds it to the `join` lists that are loaded. */
function shift(m, leave, join) {
  const key = keyOf(m);
  for (const folder of leave) state.lists[folder] = state.lists[folder].filter((x) => keyOf(x) !== key);
  for (const folder of join) {
    if (state.loaded[folder] && !state.lists[folder].some((x) => keyOf(x) === key)) {
      state.lists[folder] = withMessage(state.lists[folder], m);
    }
  }
}

function runUndo() {
  const undo = state.undo;
  if (!undo) return;
  hideToast();
  undo();
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

/** `inMessage`: the key was pressed inside the email frame, which scrolls itself. */
function onKey(e, inMessage = false) {
  if (e.ctrlKey || e.metaKey || e.altKey) return;
  if (!$('#settings').hidden || !$('#waiting').hidden) {
    if (e.key === 'Escape' && state.configured && state.accounts.length) closeSettings();
    return;
  }
  if (e.target.closest && e.target.closest('input, textarea')) return;
  switch (e.key) {
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
    case 'z': runUndo(); break;
    default: return;
  }
  e.preventDefault();
}

// ---------- Accounts & settings ----------

function openSettings() {
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

function renderSettingsAccounts() {
  $('#settings-add').disabled = !state.configured;
  $('#account-list').replaceChildren(...state.accounts.map((account) => {
    const li = document.createElement('li');
    li.style.setProperty('--acc', colorOf(account));
    li.innerHTML = '<i class="dot"></i><span></span><button>Remove</button>';
    li.querySelector('span').textContent = account;
    const button = li.querySelector('button');
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

async function addAccount() {
  if (!state.configured) { openSettings(); return; }
  const seq = ++state.authSeq;
  $('#waiting').hidden = false;
  try {
    const email = await invoke('add_account');
    if (!state.accounts.includes(email)) state.accounts.push(email);
    render();
    toast(`Added ${email}`, 2500);
    if (seq === state.authSeq) $('#settings').hidden = true;
    refresh();
  } catch (e) {
    if (seq === state.authSeq) toast(String(e), 6000);
  } finally {
    if (seq === state.authSeq) $('#waiting').hidden = true;
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
/** Shows a message at the bottom. With `action` it gets a button (Undo), which `Z` also presses. */
function toast(text, ms = 4000, action = null) {
  $('#toast-text').textContent = text;
  $('#toast-action').hidden = !action;
  $('#toast-action').textContent = action ? action.label : '';
  state.undo = action ? action.run : null;
  $('#toast').hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(hideToast, ms);
}

function hideToast() {
  $('#toast').hidden = true;
  state.undo = null;
}

function senderName(from) {
  const match = /^\s*"?([^"<]*?)"?\s*<([^>]+)>/.exec(from || '');
  if (match) return match[1].trim() || match[2];
  return (from || '').trim() || '(unknown sender)';
}

const timeFormat = new Intl.DateTimeFormat(undefined, { hour: 'numeric', minute: '2-digit' });
const dayFormat = new Intl.DateTimeFormat(undefined, { day: 'numeric', month: 'short' });
const yearFormat = new Intl.DateTimeFormat(undefined, { dateStyle: 'short' });
const longFormat = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' });

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
