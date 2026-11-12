// relevo web: the timeline page (docs/web.md). It only reads: GET /api/timeline and
// GET /api/timeline/threads/:id, through the local server, with the key of this tab.
//
// Note text is untrusted, and never parsed as HTML: `parseNote` turns it into a small tree of
// allowed nodes, and `toDom` builds DOM nodes from that tree with createElement and text nodes
// only. Links stay text.
'use strict';

// ---------- the note renderer ----------

const FENCE = /^ {0,3}(`{3,}|~{3,})/;
// After a character that is not a letter, digit, `.`, `_`, `-`, `+`, `/`, `@` or backtick.
const NOT_BEFORE_MENTION = /[A-Za-z0-9._\-+/@`]/;
const MENTION = /^@([A-Za-z0-9][A-Za-z0-9_-]{1,31})(\/[A-Za-z0-9._@/-]*)?/;
const TRAILING = /[.,;:!?)\-/']+$/;

/// A note as a list of blocks: { t: 'p', children } and { t: 'pre', text }.
function parseNote(text) {
  const lines = String(text).replace(/\r\n?/g, '\n').split('\n');
  const blocks = [];
  let para = [];
  const endPara = () => {
    if (para.length) blocks.push({ t: 'p', children: parseInline(para.join('\n')) });
    para = [];
  };
  for (let i = 0; i < lines.length; i++) {
    const open = FENCE.exec(lines[i]);
    if (open) {
      endPara();
      const mark = open[1];
      const close = new RegExp('^ {0,3}' + (mark[0] === '`' ? '`' : '~') + '{' + mark.length + ',}\\s*$');
      const code = [];
      i++;
      while (i < lines.length && !close.test(lines[i])) code.push(lines[i++]);
      blocks.push({ t: 'pre', text: code.join('\n') });
      continue;
    }
    if (lines[i].trim() === '') endPara();
    else para.push(lines[i]);
  }
  endPara();
  return blocks;
}

/// One paragraph's inline nodes: text, br, code, strong, em and mention.
function parseInline(s) {
  const out = [];
  let buf = '';
  const flush = () => {
    if (buf) out.push({ t: 'text', text: buf });
    buf = '';
  };
  let i = 0;
  while (i < s.length) {
    const c = s[i];
    if (c === '`') {
      let n = 0;
      while (s[i + n] === '`') n++;
      const close = closingTicks(s, i + n, n);
      if (close >= 0) {
        flush();
        let code = s.slice(i + n, close).replace(/\n/g, ' ');
        if (code.length > 2 && code[0] === ' ' && code[code.length - 1] === ' ' && code.trim()) {
          code = code.slice(1, -1);
        }
        out.push({ t: 'code', text: code });
        i = close + n;
      } else {
        buf += s.slice(i, i + n);
        i += n;
      }
      continue;
    }
    if (c === '*' && s[i + 1] === '*') {
      const close = s.indexOf('**', i + 2);
      if (close > i + 2 && !/\s/.test(s[i + 2]) && !/\s/.test(s[close - 1])) {
        flush();
        out.push({ t: 'strong', children: parseInline(s.slice(i + 2, close)) });
        i = close + 2;
        continue;
      }
    }
    if (c === '*' && s[i + 1] !== '*' && !/[A-Za-z0-9*]/.test(s[i - 1] || '')) {
      const close = closingStar(s, i + 1);
      if (close > i + 1 && !/\s/.test(s[i + 1]) && !/\s/.test(s[close - 1])) {
        flush();
        out.push({ t: 'em', children: parseInline(s.slice(i + 1, close)) });
        i = close + 1;
        continue;
      }
    }
    if (c === '@' && (i === 0 || !NOT_BEFORE_MENTION.test(s[i - 1]))) {
      const m = MENTION.exec(s.slice(i));
      if (m) {
        const word = m[0].replace(TRAILING, '');
        if (/^@[A-Za-z0-9][A-Za-z0-9_-]{1,31}/.test(word)) {
          flush();
          out.push({ t: 'mention', text: word });
          i += word.length;
          continue;
        }
      }
    }
    if (c === '\n') {
      flush();
      out.push({ t: 'br' });
      i++;
      continue;
    }
    buf += c;
    i++;
  }
  flush();
  return out;
}

/// Where a run of exactly `n` backticks starts, from `from` on; -1 if none.
function closingTicks(s, from, n) {
  let j = from;
  while (j < s.length) {
    if (s[j] !== '`') {
      j++;
      continue;
    }
    let m = 0;
    while (s[j + m] === '`') m++;
    if (m === n) return j;
    j += m;
  }
  return -1;
}

/// The `*` that closes an emphasis opened before `from` (not part of `**`); -1 if none.
function closingStar(s, from) {
  for (let j = from; j < s.length; j++) {
    if (s[j] === '\n' && s[j + 1] === '\n') return -1;
    if (s[j] === '*' && s[j + 1] !== '*' && s[j - 1] !== '*') return j;
  }
  return -1;
}

const TAGS = { p: 'p', pre: 'pre', code: 'code', strong: 'strong', em: 'em', mention: 'span', br: 'br' };

/// The DOM of a parsed note, built in `doc` with createElement and text nodes only.
function toDom(blocks, doc) {
  const frag = doc.createDocumentFragment();
  const add = (parent, node) => {
    if (node.t === 'text') {
      parent.appendChild(doc.createTextNode(node.text));
      return;
    }
    const tag = TAGS[node.t];
    if (!tag) return;
    const el = doc.createElement(tag);
    if (node.t === 'mention') el.className = 'mention';
    if (node.t === 'pre') {
      const code = doc.createElement('code');
      code.textContent = node.text;
      el.appendChild(code);
    } else if (node.t === 'code' || node.t === 'mention') {
      el.textContent = node.text;
    } else if (node.children) {
      node.children.forEach((child) => add(el, child));
    }
    parent.appendChild(el);
  };
  blocks.forEach((block) => add(frag, block));
  return frag;
}

/// The note `text` as a `div.md`.
function renderNote(text, doc) {
  const div = doc.createElement('div');
  div.className = 'md';
  div.appendChild(toDom(parseNote(text), doc));
  return div;
}

/// The first line that is not blank, as the server makes the title of it (docs/protocol.md:
/// trimmed, without leading `#`).
function titleLine(text) {
  const lines = String(text).split('\n');
  const first = lines.findIndex((l) => l.trim() !== '');
  return { lines, first, line: first >= 0 ? lines[first].trim().replace(/^#+/, '').trim() : '' };
}

/// The note without its first line when the thread's title was made of it (shown above it). The
/// server cuts a title to 80 characters: the line then starts with it.
function withoutTitle(text, title) {
  const { lines, first, line } = titleLine(text);
  if (title && first >= 0 && line.startsWith(title)) return lines.slice(first + 1).join('\n');
  return text;
}

const LEADING_MENTIONS = /^(?:@[A-Za-z0-9][A-Za-z0-9_-]{1,31}(?:\/[A-Za-z0-9._@/-]*)?[\s,:;]*)+/;

/// The title as shown: without the @mentions it starts with (who it is for is in the line above
/// it), and ending in '…' when the server cut it.
function shownTitle(title, text) {
  const { line } = titleLine(text);
  const cut = line.length > title.length && line.startsWith(title);
  let shown = title.replace(LEADING_MENTIONS, '');
  if (!shown.trim()) shown = title;
  else if (shown !== title) shown = shown[0].toUpperCase() + shown.slice(1);
  return shown + (cut ? '…' : '');
}

/// The key of this tab: from the address (`#k=`) when it has one, else what this tab kept.
function pickKey(hash, kept) {
  return new URLSearchParams(String(hash || '').replace(/^#/, '')).get('k') || kept || '';
}

/// A note as one paragraph, for a row's preview (the row clamps it to two lines): line breaks
/// are spaces, and a code block is its first line as inline code.
function preview(text) {
  const flat = (n) =>
    n.t === 'br' ? { t: 'text', text: ' ' } : n.children ? Object.assign({}, n, { children: n.children.map(flat) }) : n;
  const children = [];
  parseNote(text).forEach((b) => {
    if (children.length) children.push({ t: 'text', text: ' ' });
    if (b.t === 'pre') {
      const lines = b.text.split('\n');
      children.push({ t: 'code', text: lines[0] + (lines.length > 1 ? '…' : '') });
    } else {
      b.children.forEach((c) => children.push(flat(c)));
    }
  });
  return children.length ? [{ t: 'p', children }] : [];
}

/// What a row shows of the first note: what comes after its title, without the @mentions it
/// starts with.
function excerpt(text, title) {
  return withoutTitle(text, title).trim().replace(LEADING_MENTIONS, '');
}

// ---------- time ----------

const DAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];
const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

function hhmm(d) {
  return String(d.getHours()).padStart(2, '0') + ':' + String(d.getMinutes()).padStart(2, '0');
}

/// Calendar days from `d` to `now`.
function daysTo(d, now) {
  const day = (x) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  return Math.round((day(now) - day(d)) / 86400000);
}

function date(d, now) {
  return d.getDate() + ' ' + MONTHS[d.getMonth()] + (d.getFullYear() === now.getFullYear() ? '' : ' ' + d.getFullYear());
}

/// A row's time: "now", "15m", "3h", "Tue", "4 Nov", "4 Nov 2025".
function short(iso, now) {
  const d = new Date(iso);
  const secs = (now - d) / 1000;
  if (secs < 60) return 'now';
  if (secs < 3600) return Math.floor(secs / 60) + 'm';
  if (secs < 86400) return Math.floor(secs / 3600) + 'h';
  return daysTo(d, now) < 7 ? DAYS[d.getDay()] : date(d, now);
}

/// A note's time in a conversation: "16:32", "Tue 16:32", "4 Nov 16:32".
function noteTime(iso, now) {
  const d = new Date(iso);
  const days = daysTo(d, now);
  if (days === 0) return hhmm(d);
  return (days < 7 ? DAYS[d.getDay()] : date(d, now)) + ' ' + hhmm(d);
}

/// The foot's "Updated …": "just now", "2 min ago", "3 h ago".
function ago(then, now) {
  const secs = (now - then) / 1000;
  if (secs < 60) return 'just now';
  if (secs < 3600) return Math.floor(secs / 60) + ' min ago';
  return Math.floor(secs / 3600) + ' h ago';
}

function absolute(iso) {
  const d = new Date(iso);
  // In English, as the rest of the page, whatever the system's language.
  return d.toLocaleString('en-GB', { dateStyle: 'full', timeStyle: 'short' });
}

/// Every page the person has loaded, asked again: the first page, then on with `before` until past
/// the oldest thread shown. `shown` threads are shown, `shownCursor` is the cursor of the last
/// page loaded (null when it was the last page). `ask(before, limit)` answers one page, or null
/// when the answer is not wanted any more (then so is this). Returns the first answer, every
/// thread (once) and the cursor of the last page asked.
async function askShown(ask, shown, shownCursor) {
  const limit = Math.min(50, Math.max(20, shown));
  const bound = !shown ? Infinity : shownCursor ? Number(shownCursor) : -Infinity;
  const ids = new Set();
  const threads = [];
  let first = null;
  let cursor = null;
  do {
    const v = await ask(cursor, limit);
    if (!v) return null;
    if (!first) first = v;
    v.threads.forEach((t) => {
      if (!ids.has(t.id)) threads.push(t);
      ids.add(t.id);
    });
    cursor = v.cursor;
  } while (cursor && Number(cursor) > bound);
  return { first, threads, cursor };
}

/// Where the focus goes when a redraw dropped the row that had it: the row now at `at` (the one
/// that took its place), else the one before it, else nothing — the feed itself.
function focusAfter(rows, at) {
  return rows[at] || rows[at - 1] || null;
}

/// The threads *Load more* brings in: the ones not shown yet, in the order they came.
function addedThreads(known, threads) {
  return threads.filter((t) => !known.has(t.id));
}

if (typeof module === 'object' && module.exports) {
  module.exports = {
    parseNote,
    parseInline,
    toDom,
    renderNote,
    preview,
    excerpt,
    withoutTitle,
    shownTitle,
    pickKey,
    askShown,
    focusAfter,
    addedThreads,
    short,
    noteTime,
    ago,
    absolute,
  };
}

// ---------- the page ----------

if (typeof document !== 'undefined') {
  // The key of this tab. The one of the address is good for one trade: the first answer gives
  // back a session key, which the server keeps in memory and this tab in its `sessionStorage`.
  // From then on the server refuses the key of the address, so the copy the browser kept of it
  // (history, a synced session) asks for nothing. The address is cleared of it at once besides.
  const SESSION_HEADER = 'Relevo-Web-Session';
  let KEY = '';
  function keepKey(key) {
    if (!key) return;
    KEY = key;
    try {
      sessionStorage.setItem('relevo-web-key', key);
    } catch (e) {
      // Storage refused: the key lives in this page only, and a reload needs the address again.
    }
  }
  let kept = '';
  try {
    kept = sessionStorage.getItem('relevo-web-key') || '';
  } catch (e) {
    kept = '';
  }
  keepKey(pickKey(location.hash, kept));
  if (location.hash) history.replaceState(null, '', location.pathname + location.search);
  const REFRESH_MS = 30000;
  const $ = (id) => document.getElementById(id);
  const state = {
    // The view: Everyone (`board`) or For you (`threads`), with a `show` and maybe a team.
    tab: 'board',
    show: 'all',
    team: null,
    // The latest answer (counts, teams, you); For you's pending threads, shown first with All;
    // the threads of the view, their cursor, and every name known.
    data: null,
    needs: [],
    threads: [],
    cursor: null,
    names: {},
    // Conversations open, loaded, being loaded and failed; the one the person just opened.
    open: new Set(),
    convos: new Map(),
    convoLoading: new Set(),
    convoFailed: new Set(),
    opening: null,
    // Threads whose Reply needed pill has already grown in, so a refresh doesn't replay it.
    reached: new Set(),
    growing: 0,
    lastOk: null,
    lastTry: 0,
    // Each view the person picks is a new generation: an answer for an older one is dropped when
    // it comes. `busy` is the generation a load is running for, if any.
    gen: 0,
    busy: null,
    // No answer yet for this view (its old rows stay meanwhile); the last try for it failed; it
    // has taken over 300 ms.
    loading: true,
    failed: false,
    slow: false,
  };

  function el(tag, cls, text) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text !== undefined) e.textContent = text;
    return e;
  }

  function colour(handle) {
    let h = 0;
    for (const ch of handle) h = (h * 31 + ch.codePointAt(0)) >>> 0;
    return 'c' + (h % 8);
  }

  // The first character the eye sees: a whole grapheme (an emoji with its skin tone, a letter
  // with its accent), after any leading spaces.
  const graphemes = typeof Intl !== 'undefined' && Intl.Segmenter ? new Intl.Segmenter(undefined, { granularity: 'grapheme' }) : null;
  function initial(name) {
    const trimmed = String(name).trim().normalize('NFC');
    const first = graphemes ? graphemes.segment(trimmed)[Symbol.iterator]().next().value : null;
    return ((first ? first.segment : Array.from(trimmed)[0]) || '?').toUpperCase();
  }

  function avatar(handle) {
    const name = state.names[handle] || handle;
    const a = el('div', 'av ' + colour(handle), initial(name));
    a.setAttribute('aria-hidden', 'true');
    return a;
  }

  const you = () => (state.data ? state.data.you : '');
  // One name for the person, everywhere: the row, the answer and the notes of a conversation all
  // say `You`. Who wrote it is the `via …` beside it.
  const nameOf = (handle) => (handle === you() ? 'You' : state.names[handle] || handle);
  // For you with All: two groups, Needs you and The rest.
  const grouped = () => state.tab === 'threads' && state.show === 'all' && !state.team;

  /// A time; `tick` moves it on in place.
  function timeEl(iso, format) {
    const t = el('time', '', format(iso, new Date()));
    t.dateTime = iso;
    t.title = absolute(iso);
    t.format = format;
    return t;
  }

  function tick() {
    const now = new Date();
    document.querySelectorAll('time').forEach((t) => {
      const text = t.format(t.dateTime, now);
      if (t.textContent !== text) t.textContent = text;
    });
    renderFoot();
  }

  /// A short line for screen readers (the list itself is not read out again on each refresh).
  function announce(text) {
    const box = $('announce');
    box.textContent = '';
    if (text) setTimeout(() => (box.textContent = text), 50);
  }

  /// Unreachable server, wrong key, refused request: a line at the top; what is shown stays. Its
  /// space is reserved and it is a live region always rendered, so the rows never jump and the
  /// words are read out.
  function problem(err) {
    let text;
    if (err.status === 403) {
      text = "This page's key is missing or wrong: open the address relevo web printed.";
    } else if (err.status === 502 || err.status === 504) {
      text = "Can't reach the team's server.";
    } else if (err.status) {
      const msg = err.body && err.body.error && err.body.error.message;
      text = 'The server said no' + (msg ? ': ' + msg : ' (' + err.status + ')') + '.';
    } else {
      text = "Can't reach relevo web: is it still running?";
    }
    if (state.lastOk) text += ' Last updated ' + hhmm(state.lastOk) + '.';
    $('banner').textContent = text;
  }

  function fine() {
    state.lastOk = new Date();
    $('banner').textContent = '';
  }

  async function api(path) {
    let res;
    try {
      res = await fetch(path, { headers: { 'Relevo-Web-Key': KEY }, cache: 'no-store' });
    } catch (e) {
      throw { status: 0 };
    }
    // The answer to the first request carries this tab's session key: the key of the address is
    // spent with it.
    const session = res.headers.get(SESSION_HEADER);
    if (session) keepKey(session);
    let body = null;
    try {
      body = await res.json();
    } catch (e) {
      body = null;
    }
    if (!res.ok) throw { status: res.status, body };
    return body;
  }

  function query(show, before, limit) {
    const q = new URLSearchParams({ tab: state.tab });
    if (state.tab === 'threads') {
      if (show !== 'all') q.set('show', show);
      if (state.team) q.set('team', state.team);
    }
    if (before) q.set('before', before);
    if (limit) q.set('limit', String(limit));
    return '/api/timeline?' + q.toString();
  }

  /// Every page shown, asked again (so a thread far down that was closed or handled is shown so),
  /// and with For you's All every pending thread, whatever page it is on. An answer for a view no
  /// longer shown is dropped.
  async function refresh() {
    const gen = state.gen;
    if (state.busy === gen) return;
    state.busy = gen;
    state.lastTry = Date.now();
    const slow = setTimeout(() => {
      if (gen === state.gen && state.loading) {
        state.slow = true;
        renderFoot();
      }
    }, 300);
    try {
      const ask = (show) => async (before, limit) => {
        const v = await api(query(show, before, limit));
        return gen === state.gen ? v : null;
      };
      let needs = null;
      if (grouped()) {
        needs = await askShown(ask('waiting'), 50, null);
        if (!needs) return;
      }
      const shown = state.loading ? 0 : state.threads.length;
      const got = await askShown(ask(state.show), shown, state.loading ? null : state.cursor);
      if (!got) return;
      // Not on a view's first load: what is new, or has a new note, is tinted for a moment.
      const again = !state.loading;
      const before = new Map();
      if (again) state.needs.concat(state.threads).forEach((t) => before.set(t.id, t));
      const all = (needs ? needs.threads : []).concat(got.threads);
      const isNew = (t) => again && !before.has(t.id);
      const fresh = new Set(all.filter((t) => isNew(t) || (again && before.get(t.id).notes !== t.notes)).map((t) => t.id));
      const added = new Set(all.filter(isNew).map((t) => t.id)).size;
      if (state.loading) state.open.clear();
      state.data = got.first;
      state.names = Object.assign({}, state.names, needs ? needs.first.names : {}, got.first.names);
      state.needs = needs ? needs.threads : [];
      state.threads = got.threads;
      state.cursor = got.cursor;
      state.loading = state.failed = state.slow = false;
      fine();
      render(fresh);
      if (added) announce(added === 1 ? '1 new thread' : added + ' new threads');
      // A conversation open whose thread grew is read again; the copy shown stays until then.
      state.open.forEach((id) => {
        const t = all.find((x) => x.id === id);
        const was = before.get(id);
        if (!state.convos.has(id) || (t && was && (t.notes !== was.notes || t.status !== was.status))) loadConvo(id);
      });
    } catch (err) {
      if (gen !== state.gen) return;
      state.failed = true;
      problem(err);
      render();
    } finally {
      clearTimeout(slow);
      if (state.busy === gen) state.busy = null;
    }
  }

  async function loadMore() {
    const gen = state.gen;
    if (state.busy !== null || !state.cursor) return;
    // The button disables and hides itself while it works: whether it had the focus decides where
    // it goes afterwards, so that it never falls to the page body.
    const hadFocus = document.activeElement === $('loadmore');
    state.busy = gen;
    $('loadmore').disabled = true;
    // null when the answer is not wanted any more (the person changed the view).
    let added = null;
    try {
      const v = await api(query(state.show, state.cursor, 20));
      if (gen !== state.gen) return;
      added = addedThreads(new Set(state.threads.map((t) => t.id)), v.threads);
      state.threads = state.threads.concat(added);
      state.cursor = v.cursor;
      state.names = Object.assign({}, state.names, v.names);
      fine();
      render();
    } catch (err) {
      if (gen === state.gen) problem(err);
    } finally {
      if (state.busy === gen) state.busy = null;
      $('loadmore').disabled = false;
      if (hadFocus && added) {
        const first = added.length ? document.querySelector('[data-id="' + CSS.escape(added[0].id) + '"]') : null;
        const back = first || ($('loadmore').hidden ? $('feed') : $('loadmore'));
        back.focus({ preventScroll: true });
      }
    }
  }

  async function loadConvo(id) {
    if (state.convoLoading.has(id)) return;
    state.convoLoading.add(id);
    try {
      const t = await api('/api/timeline/threads/' + encodeURIComponent(id));
      state.names = Object.assign({}, state.names, t.names);
      t.loaded = Date.now();
      state.convos.set(id, t);
      state.convoFailed.delete(id);
      fine();
    } catch (err) {
      state.convoFailed.add(id);
      problem(err);
    } finally {
      state.convoLoading.delete(id);
    }
    if (state.open.has(id) && !state.loading) renderFeed();
  }

  /// Another tab or filter: a new generation. Its rows replace the old ones when they come.
  function select(tab, show, team) {
    state.gen++;
    state.tab = tab;
    state.show = show || 'all';
    state.team = team || null;
    state.loading = true;
    state.failed = state.slow = false;
    renderTabs();
    renderFilters();
    refresh();
  }

  // ---------- drawing ----------

  /// Where the thread went: "you", "@pedro", "@product", "everyone".
  function where(t) {
    if (t.kind === 'global') return 'everyone';
    if (t.kind === 'team') return t.teams.map((n) => '@' + n).join(', ');
    const others = t.participants.filter((h) => h !== t.from);
    return others.map((h) => (h === you() ? 'you' : '@' + h)).join(', ') || 'you';
  }

  /// The answer a row shows: by someone else when the person started the thread, else the latest.
  function md(text, cls) {
    const div = el('div', cls);
    div.appendChild(toDom(preview(text), document));
    return div;
  }

  /// A line of the conversation box in the message grid, under the text column; when the thread
  /// is pending, the thread line runs past it to the seat.
  function inColumn(content, pending) {
    const avCol = el('div', 'av-col');
    if (pending) {
      const lineEl = el('div', 'line joined');
      lineEl.setAttribute('aria-hidden', 'true');
      avCol.append(lineEl);
    }
    const body = el('div', 'msg-body');
    body.append(content);
    const msg = el('div', 'msg');
    msg.append(avCol, body);
    return msg;
  }

  /// The expanded conversation: replies only (the opener is already in the card's first .msg).
  function convo(t, full, pending) {
    const box = el('div', 'convo');
    box.id = 'convo-' + t.id;
    if (!full) {
      if (state.convoFailed.has(t.id) && !state.convoLoading.has(t.id)) {
        const retry = el('button', 'retry', 'Try again');
        retry.onclick = () => {
          loadConvo(t.id);
          renderFeed();
        };
        const line = el('div', 'who', "Couldn't load the conversation.");
        line.append(retry);
        box.append(inColumn(line, pending));
      } else {
        box.append(inColumn(el('div', 'who', 'Loading…'), pending));
      }
      return box;
    }
    full.notes.forEach((n, i) => {
      if (i === 0) return; // opener shown in the card's first .msg
      const avCol = el('div', 'av-col');
      avCol.append(avatar(n.author));
      // The thread line runs on from each reply to the next, and from the last to your seat
      if (pending || i < full.notes.length - 1) {
        const lineEl = el('div', 'line joined');
        lineEl.setAttribute('aria-hidden', 'true');
        avCol.append(lineEl);
      }
      const msgBody = el('div', 'msg-body');
      const who = el('div', 'who');
      who.append(el('b', '', nameOf(n.author)), el('span', '', 'via ' + n.via + (n.context ? ' in ' + n.context : '')), timeEl(n.created_at, noteTime));
      msgBody.append(who);
      if (n.text.trim()) msgBody.append(renderNote(n.text, document));
      const msg = el('div', 'msg');
      msg.append(avCol, msgBody);
      box.append(msg);
    });
    if (state.opening === t.id) {
      box.classList.add('opening');
      state.opening = null;
    }
    return box;
  }

  function row(t) {
    const open = state.open.has(t.id);
    const full = open ? state.convos.get(t.id) : null;
    const first = (full && full.notes && full.notes[0]) ? full.notes[0] : t.first_note;
    const cls = 'card' + (t.status === 'closed' ? ' closed' : '') + (open ? ' open' : '');
    const a = el('article', cls);
    a.dataset.id = t.id;
    a.setAttribute('role', 'button');
    a.setAttribute('tabindex', '0');
    a.setAttribute('aria-expanded', String(open));
    if (open) a.setAttribute('aria-controls', 'convo-' + t.id);

    // Opener message
    const hasReplies = t.notes > 1;
    const avCol = el('div', 'av-col');
    avCol.append(avatar(t.from));
    if (hasReplies || t.pending) {
      // Joined to what comes next when something is drawn below: the replies, or your seat
      const lineEl = el('div', t.pending || (open && full) ? 'line joined' : 'line');
      lineEl.setAttribute('aria-hidden', 'true');
      avCol.append(lineEl);
    }
    const msgBody = el('div', 'msg-body');
    const who = el('div', 'who');
    who.append(el('b', '', nameOf(t.from)), el('span', '', 'via ' + first.via), el('span', 'tag', where(t)));
    who.append(timeEl((t.last_note || first).created_at, short));
    msgBody.append(who);
    const noteText = withoutTitle(first.text, t.title);
    if (open) {
      const bodyText = noteText.trim() ? noteText : (t.title ? t.title + '\n\n' + first.text : first.text);
      const noteEl = renderNote(bodyText, document);
      noteEl.classList.add('text');
      msgBody.append(noteEl);
    } else {
      const textStr = (noteText.trim() ? noteText : shownTitle(t.title, first.text)) + (first.truncated ? '…' : '');
      msgBody.append(md(textStr, 'text'));
    }
    // Closed up, say how many replies the line passes over; open, they are shown instead
    if (hasReplies) {
      const n = t.notes - 1;
      msgBody.append(el('div', 'replies', n.toLocaleString('en') + (n === 1 ? ' reply' : ' replies')));
    }
    // Closed label — inside the opener, so the thread line runs past it unbroken
    if (t.status === 'closed') {
      const chips = el('div', 'chips');
      chips.append(el('span', 'label closed-label', 'Closed'));
      msgBody.append(chips);
    }
    // The status for screen readers, early in the row's name: the seat below comes after every
    // reply when the thread is open, and a long name is cut before it
    if (t.pending) msgBody.append(el('span', 'sr', ' Reply needed' + (t.kind === 'global' ? ', mentioned you' : '') + '.'));
    const openerMsg = el('div', 'msg');
    openerMsg.append(avCol, msgBody);
    a.append(openerMsg);

    // Expanded conversation — only when there are replies
    if (hasReplies) a.append(convo(t, full, t.pending));

    // Your turn: the thread line runs on to an empty seat for you, with your name and the pill.
    // It grows in the first time the row is drawn.
    if (t.pending) {
      const seat = el('div', 'av-col');
      const ghost = avatar(you());
      ghost.classList.add('ghost');
      seat.append(ghost);
      const body = el('div', 'msg-body');
      body.append(el('b', '', 'You'), el('span', 'status', 'Reply needed'));
      if (t.kind === 'global') body.append(el('span', 'label', 'mentioned you'));
      const turn = el('div', 'msg turn');
      turn.setAttribute('aria-hidden', 'true'); // said once, in the opener
      turn.append(seat, body);
      if (!state.reached.has(t.id)) {
        state.reached.add(t.id);
        turn.classList.add('grow');
        // Several at once grow 50 ms apart, up to 200 ms
        turn.style.setProperty('--d', Math.min(state.growing++, 4) * 50 + 'ms');
        // Grown: drop the class, or moving the row to another group (a filter, a tab) would
        // restart it. The body's animation ends last.
        body.addEventListener('animationend', (e) => {
          if (e.target !== body) return;
          turn.classList.remove('grow');
          turn.style.removeProperty('--d');
        });
      }
      a.append(turn);
    }

    a.onclick = (e) => {
      if (e.target.closest('.convo')) return;
      if (String(window.getSelection())) return;
      toggle(t.id);
    };
    a.onkeydown = (e) => {
      if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); toggle(t.id); }
    };
    return a;
  }

  function toggle(id) {
    if (state.loading) return;
    if (state.open.has(id)) state.open.delete(id);
    else {
      state.open.add(id);
      state.opening = id;
      const t = [...state.threads, ...state.needs].find((t) => t.id === id);
      const needsFull = !t || t.notes > 1 || (t.first_note && t.first_note.truncated);
      if (needsFull && !state.convos.has(id)) loadConvo(id);
    }
    renderFeed();
    const card = document.querySelector('[data-id="' + CSS.escape(id) + '"]');
    if (card) card.focus({ preventScroll: true });
  }

  /// What a row shows: drawn again only when it changes, so focus, a text selection and an open
  /// conversation stay as they are when nothing in that thread changed.
  function rowSig(t) {
    const notes = [t.first_note, t.last_note, t.last_other_note].filter(Boolean);
    const handles = [t.from, ...t.participants, ...notes.map((n) => n.author)];
    const open = state.open.has(t.id);
    const full = open && state.convos.get(t.id);
    const convoState = !open ? '' : full ? full.loaded : state.convoFailed.has(t.id) && !state.convoLoading.has(t.id) ? 'failed' : 'loading';
    return JSON.stringify([t, convoState, you(), handles.map((h) => state.names[h])]);
  }

  /// Puts `nodes` in `parent`, in order, moving only what must move (a node moved loses focus and
  /// selection): what is gone goes first.
  function place(parent, nodes) {
    const keep = new Set(nodes);
    for (const c of [...parent.children]) if (!keep.has(c)) c.remove();
    nodes.forEach((n, i) => {
      const at = parent.children[i];
      if (at !== n) parent.insertBefore(n, at || null);
    });
  }

  const SHOWS = [
    ['all', 'All', 'threads'],
    ['waiting', 'Needs you', 'waiting'],
    ['answered', 'Answered', 'answered'],
    ['closed', 'Closed', 'closed'],
  ];

  /// The view's name, for the heading of a list with no groups (read by screen readers only).
  function viewName() {
    if (state.tab === 'board') return 'Everyone';
    if (state.team) return 'For you: @' + state.team;
    return 'For you: ' + SHOWS.find(([s]) => s === state.show)[1];
  }

  function renderFeed(fresh) {
    const feed = $('feed');
    state.growing = 0;
    const focused = document.activeElement;
    const focusedRow = focused && focused.closest ? focused.closest('.card') : null;
    // Where the focused card sits among the cards shown, in case this redraw drops it.
    const at = focusedRow ? [...feed.querySelectorAll('.card')].indexOf(focusedRow) : -1;
    const old = new Map();
    feed.querySelectorAll('.card').forEach((r) => old.set(r.dataset.id, r));
    const rows = (threads) =>
      threads.map((t) => {
        const sig = rowSig(t);
        const was = old.get(t.id);
        if (was && was.sig === sig) return was;
        const r = row(t);
        r.sig = sig;
        if (fresh && fresh.has(t.id)) r.classList.add('fresh');
        return r;
      });
    let groups;
    if (grouped()) {
      const needs = new Set(state.needs.map((t) => t.id));
      groups = [
        ['needs', 'Needs you', state.needs],
        ['rest', 'The rest', state.threads.filter((t) => !needs.has(t.id) && !t.pending)],
      ].filter(([, , list]) => list.length);
    } else {
      groups = state.threads.length ? [['flat', viewName(), state.threads]] : [];
    }
    renderFeed.sections = renderFeed.sections || new Map();
    const sections = groups.map(([key, name, list]) => {
      let s = renderFeed.sections.get(key);
      if (!s) {
        s = el('section');
        s.append(el('h2'), el('div', 'list'));
        renderFeed.sections.set(key, s);
      }
      const h2 = s.firstChild;
      h2.className = key === 'flat' ? 'sr' : '';
      const head = [name, key === 'needs' ? list.length : ''];
      if (h2.head !== String(head)) {
        h2.head = String(head);
        h2.replaceChildren(name + (key === 'needs' ? ' ' : ''));
        if (key === 'needs') h2.append(el('span', 'n', String(list.length)));
      }
      place(s.lastChild, rows(list));
      return s;
    });
    place(feed, sections);
    // The row that had focus, if it was drawn again; else the row that took its place, or the one
    // before it, or the feed: focus never falls to the page body.
    if (focusedRow && !focusedRow.isConnected) {
      const again = document.querySelector('[data-id="' + CSS.escape(focusedRow.dataset.id) + '"]');
      if (again) {
        again.focus({ preventScroll: true });
      } else {
        const near = focusAfter([...feed.querySelectorAll('.card')], at);
        const next = near || feed;
        next.focus({ preventScroll: true });
      }
    }
  }

  function renderTabs() {
    for (const [tab, id] of [['board', 'tab-board'], ['threads', 'tab-threads']]) {
      const on = state.tab === tab;
      $(id).setAttribute('aria-selected', String(on));
      $(id).tabIndex = on ? 0 : -1;
      if (on) $('feed').setAttribute('aria-labelledby', id);
    }
    const n = state.data ? state.data.counts.waiting : 0;
    $('for-you-n').textContent = n > 0 ? String(n) : '';
  }

  /// For you's filters, drawn again only when they change; the one focused keeps the focus.
  function renderFilters() {
    const v = state.data;
    const nav = $('filters');
    nav.hidden = state.tab !== 'threads' || !v;
    if (nav.hidden) return;
    const sig = JSON.stringify([v.teams, v.counts, state.show, state.team]);
    if (sig === nav.sig) return;
    nav.sig = sig;
    const focused = nav.contains(document.activeElement) ? document.activeElement.dataset.f : null;
    const button = (key, label, n, on, pick) => {
      const b = el('button', '', label + ' ');
      b.append(el('b', '', String(n)));
      b.dataset.f = key;
      b.setAttribute('aria-pressed', String(on));
      b.onclick = pick;
      return b;
    };
    const shows = SHOWS.map(([show, label, count]) =>
      button(show, label, v.counts[count], !state.team && state.show === show, () => select('threads', show))
    );
    const teams = v.teams.map((t) =>
      button('@' + t, '@' + t, v.counts.teams[t] || 0, state.team === t, () => select('threads', 'all', t))
    );
    const sep = el('span', 'sep');
    sep.setAttribute('aria-hidden', 'true');
    nav.replaceChildren(...shows, ...(teams.length ? [sep, ...teams] : []));
    if (focused) {
      const again = nav.querySelector('[data-f="' + CSS.escape(focused) + '"]');
      if (again) again.focus();
    }
  }

  function renderFoot() {
    const foot = $('foot');
    const text = state.loading && state.slow ? 'Loading…' : state.lastOk ? 'Updated ' + ago(state.lastOk, new Date()) : '';
    if (foot.textContent !== text) foot.textContent = text;
    foot.hidden = !text;
  }

  function render(fresh) {
    renderTabs();
    renderFilters();
    if (state.loading && state.failed) {
      // Not the old view's rows under the new one's name.
      $('feed').replaceChildren();
    } else if (!state.loading) {
      renderFeed(fresh);
    }
    const empty = $('empty');
    let text = '';
    if (state.loading && state.failed) text = "Couldn't load this. It is tried again every 30 seconds.";
    else if (!state.loading && !state.threads.length && !state.needs.length) {
      text = state.tab === 'board' ? 'Nothing on the board yet.' : grouped() ? 'Nothing for you yet.' : 'Nothing here.';
    }
    empty.textContent = text;
    empty.hidden = !text;
    $('loadmore').hidden = state.loading || !state.cursor;
    renderFoot();
  }

  $('tab-board').onclick = () => select('board');
  $('tab-threads').onclick = () => select('threads');
  // The tabs, as a tablist: the arrow keys (and Home, End) move between them.
  $('tabs').addEventListener('keydown', (e) => {
    const tabs = ['tab-board', 'tab-threads'];
    const at = tabs.indexOf(e.target.id);
    if (at < 0) return;
    let to = null;
    if (e.key === 'ArrowRight') to = (at + 1) % tabs.length;
    else if (e.key === 'ArrowLeft') to = (at + tabs.length - 1) % tabs.length;
    else if (e.key === 'Home') to = 0;
    else if (e.key === 'End') to = tabs.length - 1;
    if (to === null) return;
    e.preventDefault();
    $(tabs[to]).focus();
    if (to !== at) select(to === 0 ? 'board' : 'threads');
  });
  $('foot').onclick = () => refresh();
  $('loadmore').onclick = () => loadMore();
  setInterval(() => {
    if (document.visibilityState === 'visible') refresh();
  }, REFRESH_MS);
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'visible' && Date.now() - state.lastTry > REFRESH_MS) refresh();
  });
  // Times move on even when nothing new comes, in place.
  setInterval(() => {
    if (document.visibilityState === 'visible') tick();
  }, 30000);
  render();
  refresh();
}
