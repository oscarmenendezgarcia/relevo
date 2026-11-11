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
function renderNote(text, doc, extra) {
  const div = doc.createElement('div');
  div.className = 'md' + (extra ? ' ' + extra : '');
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

// ---------- time ----------

const DAYS = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'];
const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

function hhmm(d) {
  return String(d.getHours()).padStart(2, '0') + ':' + String(d.getMinutes()).padStart(2, '0');
}

/// "just now", "12 min ago", "2 h ago", "14:05", "yesterday 19:25", "Monday", "3 Nov".
function relative(iso, now) {
  const d = new Date(iso);
  const secs = (now - d) / 1000;
  if (secs < 60) return 'just now';
  if (secs < 3600) return Math.floor(secs / 60) + ' min ago';
  if (secs < 6 * 3600) return Math.floor(secs / 3600) + ' h ago';
  const day = (x) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const days = Math.round((day(now) - day(d)) / 86400000);
  if (days === 0) return hhmm(d);
  if (days === 1) return 'yesterday ' + hhmm(d);
  if (days < 7) return DAYS[d.getDay()];
  const year = d.getFullYear() === now.getFullYear() ? '' : ' ' + d.getFullYear();
  return d.getDate() + ' ' + MONTHS[d.getMonth()] + year;
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

if (typeof module === 'object' && module.exports) {
  module.exports = {
    parseNote,
    parseInline,
    toDom,
    renderNote,
    withoutTitle,
    shownTitle,
    pickKey,
    askShown,
    relative,
    absolute,
  };
}

// ---------- the page ----------

if (typeof document !== 'undefined') {
  // The key stays in this tab (sessionStorage: it survives a reload) and leaves the address at
  // once, so it is not kept in the history, restored with the session, synced or shared.
  const KEY = (() => {
    let kept = '';
    try {
      kept = sessionStorage.getItem('relevo-web-key') || '';
    } catch (e) {
      kept = '';
    }
    const key = pickKey(location.hash, kept);
    try {
      if (key) sessionStorage.setItem('relevo-web-key', key);
    } catch (e) {
      // Storage refused: the key lives in this page only, and a reload needs the address again.
    }
    if (location.hash) history.replaceState(null, '', location.pathname + location.search);
    return key;
  })();
  const REFRESH_MS = 30000;
  const $ = (id) => document.getElementById(id);
  const state = {
    tab: 'board',
    filter: { show: 'all' },
    // The latest answer (for the side bar), and the threads of the view shown.
    data: null,
    threads: [],
    cursor: null,
    names: {},
    open: new Set(),
    convos: new Map(),
    // Conversations being loaded, and those whose last load failed.
    convoLoading: new Set(),
    convoFailed: new Set(),
    lastOk: null,
    lastTry: 0,
    // Each view (tab and filter) the person picks is a new generation: an answer for an older
    // one is dropped when it comes. `busy` is the generation a load is running for, if any.
    gen: 0,
    busy: null,
    // No answer yet for this view; and whether the last try for it failed.
    loading: true,
    failed: false,
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

  function avatar(handle) {
    const name = state.names[handle] || handle;
    const a = el('div', 'av ' + colour(handle), (Array.from(name)[0] || '?').toUpperCase());
    a.setAttribute('aria-hidden', 'true');
    return a;
  }

  const you = () => (state.data ? state.data.you : '');
  const nameOf = (handle) => (handle === you() ? 'You' : state.names[handle] || handle);

  function via(note) {
    return note.via + (note.context ? ' in ' + note.context : '');
  }

  /// A time, relative; `tick` moves it on in place.
  function timeEl(iso, cls) {
    const t = el('span', cls, relative(iso, new Date()));
    t.dataset.at = iso;
    t.title = absolute(iso);
    return t;
  }

  function tick() {
    const now = new Date();
    document.querySelectorAll('[data-at]').forEach((t) => {
      const text = relative(t.dataset.at, now);
      if (t.textContent !== text) t.textContent = text;
    });
  }

  /// A short line for screen readers (the feed itself is not read out again on each refresh).
  function announce(text) {
    const box = $('announce');
    box.textContent = '';
    if (text) setTimeout(() => (box.textContent = text), 50);
  }

  /// Unreachable server, wrong key, refused request: a line at the top; what is shown stays.
  function problem(err) {
    const banner = $('banner');
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
    banner.textContent = text;
    banner.hidden = false;
  }

  function fine() {
    state.lastOk = new Date();
    $('banner').hidden = true;
  }

  async function api(path) {
    let res;
    try {
      res = await fetch(path, { headers: { 'Relevo-Web-Key': KEY }, cache: 'no-store' });
    } catch (e) {
      throw { status: 0 };
    }
    let body = null;
    try {
      body = await res.json();
    } catch (e) {
      body = null;
    }
    if (!res.ok) throw { status: res.status, body };
    return body;
  }

  function query(before, limit) {
    const q = new URLSearchParams({ tab: state.tab });
    if (state.tab === 'threads') {
      if (state.filter.show && state.filter.show !== 'all') q.set('show', state.filter.show);
      if (state.filter.team) q.set('team', state.filter.team);
      if (state.filter.with) q.set('with', state.filter.with);
    }
    if (before) q.set('before', before);
    if (limit) q.set('limit', String(limit));
    return '/api/timeline?' + q.toString();
  }

  function setBusy() {
    $('refresh').classList.toggle('busy', state.busy !== null);
    $('loadmore').disabled = state.busy !== null;
  }

  /// Every page shown, asked again (so a thread far down that was closed or handled is shown so),
  /// for the view shown; an answer for a view no longer shown is dropped.
  async function refresh() {
    const gen = state.gen;
    if (state.busy === gen) return;
    state.busy = gen;
    state.lastTry = Date.now();
    setBusy();
    try {
      const ask = async (before, limit) => {
        const v = await api(query(before, limit));
        return gen === state.gen ? v : null;
      };
      const got = await askShown(ask, state.threads.length, state.cursor);
      if (!got) return;
      const before = new Map(state.threads.map((t) => [t.id, t]));
      const added = state.loading ? 0 : got.threads.filter((t) => !before.has(t.id)).length;
      state.data = got.first;
      state.names = Object.assign({}, state.names, got.first.names);
      state.threads = got.threads;
      state.cursor = got.cursor;
      state.loading = false;
      state.failed = false;
      fine();
      render();
      if (added) announce(added === 1 ? '1 new thread' : added + ' new threads');
      // A conversation open whose thread grew is read again; the copy shown stays until then.
      state.open.forEach((id) => {
        const t = got.threads.find((x) => x.id === id);
        const was = before.get(id);
        const grew = t && was && (t.notes !== was.notes || t.status !== was.status);
        if (!state.convos.has(id) || grew) loadConvo(id);
      });
    } catch (err) {
      if (gen !== state.gen) return;
      state.failed = true;
      problem(err);
      render();
    } finally {
      if (state.busy === gen) state.busy = null;
      setBusy();
    }
  }

  async function loadMore() {
    const gen = state.gen;
    if (state.busy !== null || !state.cursor) return;
    state.busy = gen;
    setBusy();
    try {
      const v = await api(query(state.cursor, 20));
      if (gen !== state.gen) return;
      const ids = new Set(state.threads.map((t) => t.id));
      state.threads = state.threads.concat(v.threads.filter((t) => !ids.has(t.id)));
      state.cursor = v.cursor;
      state.names = Object.assign({}, state.names, v.names);
      fine();
      render();
    } catch (err) {
      if (gen === state.gen) problem(err);
    } finally {
      if (state.busy === gen) state.busy = null;
      setBusy();
    }
  }

  async function loadConvo(id) {
    if (state.convoLoading.has(id)) return;
    state.convoLoading.add(id);
    try {
      const t = await api('/api/timeline/threads/' + encodeURIComponent(id));
      state.names = Object.assign({}, state.names, t.names);
      state.convos.set(id, t);
      state.convoFailed.delete(id);
      fine();
    } catch (err) {
      state.convoFailed.add(id);
      problem(err);
    } finally {
      state.convoLoading.delete(id);
    }
    const box = document.getElementById(convoId(id));
    if (box && state.open.has(id)) fillConvo(box, id);
  }

  /// Another tab or filter: a new generation, and its first page.
  function select(tab, filter) {
    state.gen++;
    state.tab = tab;
    if (filter) state.filter = filter;
    state.threads = [];
    state.cursor = null;
    state.open.clear();
    state.loading = true;
    state.failed = false;
    render();
    refresh();
  }

  // ---------- drawing ----------

  function filterButton(label, count, on, onclick) {
    const b = el('button', 'filter' + (on ? ' on' : ''));
    b.append(el('span', '', label));
    if (count !== undefined) b.append(el('b', '', String(count)));
    b.setAttribute('aria-pressed', on ? 'true' : 'false');
    b.onclick = onclick;
    return b;
  }

  function renderSide() {
    const v = state.data;
    if (!v) return;
    const me = $('me');
    const teams = v.teams.length ? ' · in ' + v.teams.map((t) => '@' + t).join(', ') : '';
    const who = el('div', 'who', state.names[v.you] || v.you);
    who.append(el('small', '', '@' + v.you + teams));
    me.replaceChildren(avatar(v.you), who);
    me.hidden = false;

    const inThreads = state.tab === 'threads';
    const f = state.filter;
    const show = [
      ['all', 'All', v.counts.threads],
      ['waiting', 'Waiting for you', v.counts.waiting],
      ['answered', 'Answered by your agent', v.counts.answered],
      ['closed', 'Closed', v.counts.closed],
    ].map(([value, label, n]) =>
      filterButton(label, n, inThreads && f.show === value && !f.team && !f.with, () =>
        select('threads', { show: value })
      )
    );
    const teams$ = v.teams.map((t) =>
      filterButton('@' + t, v.counts.teams[t] || 0, inThreads && f.team === t, () =>
        select('threads', { show: 'all', team: t })
      )
    );
    const people = v.people.map((h) =>
      filterButton('@' + h, undefined, inThreads && f.with === h, () =>
        select('threads', { show: 'all', with: h })
      )
    );
    // Drawn again only when something in it changed, so a focused filter keeps its focus.
    const sig = JSON.stringify([v.you, v.teams, v.counts, v.people, state.names[v.you], state.tab, f]);
    if (sig === renderSide.sig) return;
    renderSide.sig = sig;
    $('show').replaceChildren(...show);
    $('teams').replaceChildren(...(teams$.length ? teams$ : [el('div', 'none', 'No team')]));
    $('people').replaceChildren(...(people.length ? people : [el('div', 'none', 'Nobody yet')]));
  }

  /// "@pedro → you", "@luis → @product", "@oscar → everyone".
  function target(t) {
    if (t.kind === 'global') return 'everyone';
    if (t.kind === 'team') return t.teams.map((n) => '@' + n).join(', ');
    const others = t.participants.filter((h) => h !== t.from);
    return others.map((h) => (h === you() ? 'you' : '@' + h)).join(', ') || 'you';
  }

  function replies(n) {
    return n === 1 ? '1 reply' : n + ' replies';
  }

  function chips(t) {
    const out = [];
    const me = you();
    const last = t.last_note || t.first_note;
    if (t.kind === 'global') {
      if (t.pending) out.push(['pend', 'mentions you · waiting for you']);
      if (t.status === 'closed') out.push(['done', 'resolved']);
      if (t.notes > 1) out.push(['', replies(t.notes - 1)]);
      else out.push(['', t.status === 'open' ? 'open · no replies' : 'no replies']);
      return out;
    }
    if (t.pending) out.push(['pend', 'waiting for you']);
    t.teams.forEach((n) => out.push(['team', 'to @' + n]));
    if (t.from !== me && t.handled_by_you && !t.pending) {
      out.push(['done', last && last.author === me ? 'answered by your agent' : 'handled by your agent']);
    }
    if (t.from === me) {
      // Someone answered: the latest note is theirs, or you handled (or have pending) a note by
      // someone else. In a direct thread of two, that is the other person.
      const other = t.participants.filter((h) => h !== me);
      if (t.last_note && t.last_note.author !== me) {
        out.push(['done', nameOf(t.last_note.author) + ' answered']);
      } else if (t.handled_by_you || t.pending) {
        const who = t.kind === 'direct' && other.length === 1 ? nameOf(other[0]) + ' answered' : 'answered';
        out.push(['done', who]);
      }
    }
    if (t.kind === 'team' && !t.named && t.from !== me) out.push(['', 'optional for you']);
    if (t.status === 'closed') {
      if (t.kind === 'team') out.push(['done', 'resolved']);
      else if (t.closed_by === me) out.push(['closed', 'closed by your agent']);
      else if (t.closed_by) out.push(['closed', 'closed by ' + nameOf(t.closed_by)]);
      else out.push(['closed', 'closed']);
    } else if (t.kind === 'direct' && !t.pending) {
      out.push(['', 'open']);
    }
    if (t.notes > 1) out.push(['', replies(t.notes - 1)]);
    return out;
  }

  const convoId = (id) => 'convo-' + id;

  /// The conversation of thread `id` in `box`: the copy loaded (kept while a newer one loads),
  /// else a line saying it loads, or that it could not.
  function fillConvo(box, id) {
    const t = state.convos.get(id);
    if (!t) {
      if (state.convoFailed.has(id) && !state.convoLoading.has(id)) {
        const retry = el('button', 'retry', 'Try again');
        retry.onclick = (e) => {
          // The button is gone once the box is redrawn: the post must not take the click.
          e.stopPropagation();
          loadConvo(id);
          fillConvo(box, id);
        };
        box.replaceChildren(el('div', 'meta', "Couldn't load the conversation. "), retry);
      } else {
        box.replaceChildren(el('div', 'meta', 'Loading the conversation…'));
      }
      return;
    }
    if (box.shown === t) return;
    box.shown = t;
    box.replaceChildren(
      ...t.notes.map((n) => {
        const row = el('div', 'note');
        const bubble = el('div', 'bubble');
        const meta = el('div', 'meta');
        meta.append(el('b', '', nameOf(n.author)), ' · ' + via(n) + ' · ', timeEl(n.created_at));
        bubble.append(meta, renderNote(n.text, document));
        row.append(avatar(n.author), bubble);
        return row;
      })
    );
  }

  function moreLabel(t, open) {
    if (open) return 'Hide conversation';
    return t.notes === 1 ? 'Read the whole note' : 'See conversation · ' + t.notes + ' notes';
  }

  function post(t) {
    const open = state.open.has(t.id);
    const a = el('article', 'post');
    a.dataset.id = t.id;
    const first = t.first_note;
    const content = el('div', 'content');

    const head = el('div', 'head');
    head.append(el('b', '', nameOf(t.from)), el('span', 'h', '@' + t.from + ' → ' + target(t)));
    head.append(timeEl(first.created_at, 't'));
    content.append(head, el('div', 'via', 'via ' + via(first)));
    // The title is the post's control: a button, for the keyboard and screen readers.
    const title = el('button', 'title', shownTitle(t.title, first.text));
    title.setAttribute('aria-expanded', open ? 'true' : 'false');
    title.setAttribute('aria-controls', convoId(t.id));
    content.append(title);
    const rest = withoutTitle(first.text, t.title);
    if (rest.trim()) {
      content.append(renderNote(rest + (first.truncated ? '…' : ''), document, 'body clip'));
    }
    // The answer: the latest note by someone else, never the person's own (docs/web.md).
    const answer = t.last_other_note;
    if (answer) {
      const n = answer;
      const reply = el('div', 'reply');
      const rh = el('div', 'head');
      rh.append(el('b', '', nameOf(n.author)), el('span', 'h', 'via ' + via(n)), timeEl(n.created_at, 't'));
      reply.append(rh, renderNote(n.text + (n.truncated ? '…' : ''), document, 'body clip'));
      content.append(reply);
    }
    const chipBox = el('div', 'chips');
    chips(t).forEach(([cls, text]) => chipBox.append(el('span', 'chip ' + cls, text)));
    content.append(chipBox);
    // Shown when there is more than the preview; `markCut` adds it too when a preview is cut by
    // its height.
    if (open || t.notes > 2 || first.truncated || (answer && answer.truncated)) {
      content.append(el('div', 'more', moreLabel(t, open)));
    }
    const box = el('div', 'convo');
    box.id = convoId(t.id);
    box.hidden = !open;
    if (open) fillConvo(box, t.id);
    content.append(box);
    a.append(avatar(t.from), content);
    a.onclick = (e) => {
      if (e.target.closest('.convo')) return;
      if (e.target !== title && String(window.getSelection())) return;
      toggle(t.id);
    };
    return a;
  }

  function toggle(id) {
    if (state.open.has(id)) state.open.delete(id);
    else {
      state.open.add(id);
      if (!state.convos.has(id)) loadConvo(id);
    }
    render();
    const again = document.querySelector('[data-id="' + CSS.escape(id) + '"] .title');
    if (again) again.focus({ preventScroll: true });
  }

  /// Previews cut by their height fade out at the bottom, and say the post can be opened.
  function markCut(root) {
    root.querySelectorAll('.post').forEach((p) => {
      let cut = false;
      p.querySelectorAll('.clip').forEach((c) => {
        const is = c.scrollHeight > c.clientHeight + 1;
        c.classList.toggle('cut', is);
        cut = cut || is;
      });
      const content = p.querySelector('.content');
      if (cut && !content.querySelector('.more')) {
        const t = state.threads.find((x) => x.id === p.dataset.id);
        if (t) content.insertBefore(el('div', 'more', moreLabel(t, false)), content.querySelector('.convo'));
      }
    });
  }

  /// What a post shows: drawn again only when it changes, so focus, a text selection and an open
  /// conversation stay as they are when nothing in that thread changed.
  function postSig(t) {
    const notes = [t.first_note, t.last_note, t.last_other_note].filter(Boolean);
    const handles = [t.from, t.closed_by, ...t.participants, ...notes.map((n) => n.author)];
    const names = handles.map((h) => h && state.names[h]);
    return JSON.stringify([t, state.open.has(t.id), you(), names]);
  }

  function renderFeed() {
    const feed = $('feed');
    const focused = document.activeElement;
    const focusedPost = focused && focused.closest ? focused.closest('.post') : null;
    const old = new Map();
    for (const p of feed.children) old.set(p.dataset.id, p);
    const nodes = state.threads.map((t) => {
      const sig = postSig(t);
      const was = old.get(t.id);
      if (was && was.sig === sig) return was;
      const p = post(t);
      p.sig = sig;
      return p;
    });
    // Takes out what is gone or redrawn first, so the posts that stay never move (moving a node
    // detaches it, and it loses focus and selection) unless the order changed.
    const keep = new Set(nodes);
    for (const p of [...feed.children]) if (!keep.has(p)) p.remove();
    nodes.forEach((n, i) => {
      const at = feed.children[i];
      if (at !== n) feed.insertBefore(n, at || null);
    });
    while (feed.children.length > nodes.length) feed.lastElementChild.remove();
    // The post that had focus, if it was drawn again.
    if (focusedPost && !focusedPost.isConnected) {
      const again = feed.querySelector('[data-id="' + CSS.escape(focusedPost.dataset.id) + '"] .title');
      if (again) again.focus({ preventScroll: true });
    }
    markCut(feed);
  }

  function renderTabs() {
    for (const [tab, id] of [['board', 'tab-board'], ['threads', 'tab-threads']]) {
      const on = state.tab === tab;
      $(id).classList.toggle('on', on);
      $(id).setAttribute('aria-selected', String(on));
      $(id).tabIndex = on ? 0 : -1;
      if (on) $('feed').setAttribute('aria-labelledby', id);
    }
  }

  function render() {
    renderTabs();
    renderSide();
    renderFeed();
    $('loadmore').hidden = !state.cursor;
    const footer = $('footer');
    let text = '';
    if (state.loading) {
      text = state.failed ? "Couldn't load this view. It is tried again every 30 seconds, or with ↻." : 'Loading…';
    } else if (state.cursor) {
      text = '';
    } else if (state.tab === 'board') {
      text = state.threads.length
        ? 'Notes with no @, for everyone. Team notes and direct notes are seen only by the people in them.'
        : 'Nothing here yet. A note with no @ goes here, for everyone to see.';
    } else {
      text = state.threads.length ? '' : 'No threads here.';
    }
    footer.textContent = text;
    footer.hidden = !text;
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
  $('refresh').onclick = () => refresh();
  $('loadmore').onclick = () => loadMore();
  setInterval(() => {
    if (document.visibilityState === 'visible') refresh();
  }, REFRESH_MS);
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'visible' && Date.now() - state.lastTry > REFRESH_MS) refresh();
  });
  // Relative times move on even when nothing new comes, in place.
  setInterval(() => {
    if (document.visibilityState === 'visible') tick();
  }, 60000);
  render();
  refresh();
}
