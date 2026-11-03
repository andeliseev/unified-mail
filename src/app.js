'use strict';

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const PALETTE = ['#2f6df6', '#d93025', '#188038', '#e37400', '#9334e6', '#00897b', '#c2185b', '#5f6368'];
// Push (IMAP IDLE) triggers syncs instantly; polling is only a safety net.
const AUTO_REFRESH_MS = 5 * 60 * 1000;
// Gmail often sends several push lines per change; sync once for the burst.
const PUSH_DEBOUNCE_MS = 300;
const REFRESH_ON_FOCUS_AFTER_MS = 30 * 1000;
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

const state = {
  builtInClient: false,  // OAuth client compiled into the app; Settings hides that section
  configured: false,
  accounts: [],
  messages: [],
  filter: null,          // account email, or null for all inboxes
  selected: null,        // key of the highlighted row
  current: null,         // message shown in the reader
  pending: new Set(),    // keys being archived/deleted; hidden from sync results
  syncing: false,
  syncQueued: false,
  lastSync: 0,
  openSeq: 0,
  authSeq: 0,
};

const $ = (sel) => document.querySelector(sel);
const keyOf = (m) => `${m.account}/${m.id}`;
const colorOf = (account) => PALETTE[Math.max(0, state.accounts.indexOf(account)) % PALETTE.length];
const visible = () => (state.filter ? state.messages.filter((m) => m.account === state.filter) : state.messages);
const byKey = (key) => state.messages.find((m) => keyOf(m) === key);

// ---------- Startup ----------

async function init() {
  bindEvents();
  const setup = await invoke('get_setup');
  state.builtInClient = setup.builtInClient;
  state.configured = setup.builtInClient || Boolean(setup.clientId);
  state.accounts = setup.accounts;
  $('#client-section').hidden = setup.builtInClient;
  $('#client-id').value = setup.clientId || '';
  state.messages = await invoke('cached_inbox');
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

async function refresh() {
  if (!state.accounts.length) return;
  if (state.syncing) { state.syncQueued = true; return; }
  state.syncing = true;
  $('#refresh').classList.add('spin');
  setStatus('Syncing…');
  try {
    const result = await invoke('refresh_inbox');
    state.messages = result.messages.filter((m) => !state.pending.has(keyOf(m)));
    state.lastSync = Date.now();
    render();
    setStatus('');
    if (result.errors.length) toast(result.errors.map((e) => `${e.account}: ${e.message}`).join('\n'), 8000);
  } catch (e) {
    setStatus('');
    toast(String(e));
  } finally {
    state.syncing = false;
    $('#refresh').classList.remove('spin');
    if (state.syncQueued) { state.syncQueued = false; refresh(); }
  }
}

// ---------- Rendering ----------

function render() {
  renderSidebar();
  renderList();
}

function renderSidebar() {
  const unread = new Map();
  for (const m of state.messages) if (m.unread) unread.set(m.account, (unread.get(m.account) || 0) + 1);
  const total = [...unread.values()].reduce((a, b) => a + b, 0);
  const items = [{ account: null, label: 'All inboxes', count: total }].concat(
    state.accounts.map((a) => ({ account: a, label: a, count: unread.get(a) || 0 })));

  $('#accounts').replaceChildren(...items.map((item) => {
    const button = document.createElement('button');
    button.className = 'nav-item' + (item.account ? '' : ' all') + (state.filter === item.account ? ' active' : '');
    button.innerHTML = '<span class="dot"></span><span class="label"></span><span class="count"></span>';
    if (item.account) button.style.setProperty('--acc', colorOf(item.account));
    button.querySelector('.label').textContent = item.label;
    button.querySelector('.count').textContent = item.count || '';
    button.title = item.label;
    button.addEventListener('click', () => { state.filter = item.account; render(); });
    return button;
  }));
  $('#list-title').textContent = state.filter || 'All inboxes';
  renderSettingsAccounts();
}

function renderList() {
  const messages = visible();
  const fragment = document.createDocumentFragment();
  for (const m of messages) {
    const li = document.createElement('li');
    li.className = 'msg' + (m.unread ? ' unread' : '') + (keyOf(m) === state.selected ? ' selected' : '');
    li.dataset.key = keyOf(m);
    li.style.setProperty('--acc', colorOf(m.account));
    li.innerHTML = '<div class="row1"><span class="from"></span><time></time></div><div class="subject"></div><div class="snippet"></div>';
    li.querySelector('.from').textContent = senderName(m.from);
    li.querySelector('time').textContent = shortDate(m.date);
    li.querySelector('.subject').textContent = m.subject || '(no subject)';
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
      : state.syncing && state.lastSync === 0 ? 'Loading…' : 'Inbox zero. Nothing here.';
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

// ---------- Reading ----------

async function select(key) {
  const m = byKey(key);
  if (!m) return;
  state.selected = key;
  state.current = m;
  markRow(key);

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
      m.unread = false;
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

/** Archive or trash the open message. Optimistic: the row disappears right away. */
async function act(kind) {
  const m = state.current;
  if (!m) return;
  const key = keyOf(m);
  const list = visible();
  const index = list.findIndex((x) => keyOf(x) === key);
  const next = index >= 0 ? list[index + 1] || list[index - 1] : null;

  state.pending.add(key);
  state.messages = state.messages.filter((x) => keyOf(x) !== key);
  render();
  if (next) select(keyOf(next)); else clearReader();

  try {
    await invoke(kind === 'archive' ? 'archive_message' : 'trash_message', { account: m.account, id: m.id });
    toast(kind === 'archive' ? 'Archived' : 'Moved to Trash', 1800);
  } catch (e) {
    state.messages.push(m);
    state.messages.sort((a, b) => b.date - a.date);
    render();
    toast(`Couldn't ${kind === 'archive' ? 'archive' : 'delete'} the message: ${e}`, 6000);
  } finally {
    state.pending.delete(key);
  }
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
        state.messages = state.messages.filter((m) => m.account !== account);
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
function toast(text, ms = 4000) {
  const el = $('#toast');
  el.textContent = text;
  el.style.whiteSpace = 'pre-line';
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { el.hidden = true; }, ms);
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
