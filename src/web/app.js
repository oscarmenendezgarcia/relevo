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

/// The note without its first line when that line is the thread's title (shown above it).
function withoutTitle(text, title) {
  const lines = String(text).split('\n');
  const first = lines.findIndex((l) => l.trim() !== '');
  if (first >= 0 && lines[first].trim().replace(/^#+/, '').trim() === title) {
    return lines.slice(first + 1).join('\n');
  }
  return text;
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
  return d.toLocaleString(undefined, { dateStyle: 'full', timeStyle: 'short' });
}

if (typeof module === 'object' && module.exports) {
  module.exports = { parseNote, parseInline, toDom, renderNote, withoutTitle, relative };
}

// ---------- the page ----------

if (typeof document !== 'undefined') {
  const KEY = new URLSearchParams(location.hash.slice(1)).get('k') || '';
  const REFRESH_MS = 30000;
  const $ = (id) => document.getElementById(id);
  const state = {
    tab: 'board',
    filter: { show: 'all' },
    data: null,
    threads: [],
    cursor: null,
    names: {},
    open: new Set(),
    convos: new Map(),
    lastOk: null,
    lastTry: 0,
    busy: false,
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

  function timeEl(iso) {
    const t = el('span', 't', relative(iso, new Date()));
    t.title = absolute(iso);
    return t;
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

  /// The first page again (as many threads as are shown, up to 50); older pages already loaded
  /// stay below it.
  async function refresh() {
    if (state.busy) return;
    state.busy = true;
    state.lastTry = Date.now();
    $('refresh').classList.add('busy');
    const asked = state.tab + JSON.stringify(state.filter);
    try {
      const shown = state.threads.length;
      const limit = Math.min(50, Math.max(20, shown));
      const v = await api(query(null, limit));
      if (asked !== state.tab + JSON.stringify(state.filter)) return;
      const fresh = v.threads;
      const ids = new Set(fresh.map((t) => t.id));
      const oldest = fresh.length ? Number(fresh[fresh.length - 1].cursor) : Infinity;
      const rest = v.cursor ? state.threads.filter((t) => !ids.has(t.id) && Number(t.cursor) < oldest) : [];
      // A conversation open whose thread grew is read again.
      for (const t of fresh) {
        const before = state.threads.find((o) => o.id === t.id);
        if (state.open.has(t.id) && before && before.notes !== t.notes) state.convos.delete(t.id);
      }
      state.data = v;
      state.names = Object.assign({}, state.names, v.names);
      state.threads = fresh.concat(rest);
      state.cursor = rest.length ? state.cursor : v.cursor;
      state.lastOk = new Date();
      $('banner').hidden = true;
      render();
      state.open.forEach((id) => {
        if (!state.convos.has(id)) loadConvo(id);
      });
    } catch (err) {
      problem(err);
    } finally {
      state.busy = false;
      $('refresh').classList.remove('busy');
    }
  }

  async function loadMore() {
    const button = $('loadmore');
    button.disabled = true;
    try {
      const v = await api(query(state.cursor, 20));
      const ids = new Set(state.threads.map((t) => t.id));
      state.threads = state.threads.concat(v.threads.filter((t) => !ids.has(t.id)));
      state.cursor = v.cursor;
      state.names = Object.assign({}, state.names, v.names);
      state.lastOk = new Date();
      $('banner').hidden = true;
      render();
    } catch (err) {
      problem(err);
    } finally {
      button.disabled = false;
    }
  }

  async function loadConvo(id) {
    try {
      const t = await api('/api/timeline/threads/' + encodeURIComponent(id));
      state.names = Object.assign({}, state.names, t.names);
      state.convos.set(id, t);
      $('banner').hidden = true;
      const post = document.querySelector('[data-id="' + CSS.escape(id) + '"]');
      if (post) fillConvo(post.querySelector('.convo'), t);
    } catch (err) {
      problem(err);
    }
  }

  function select(tab, filter) {
    state.tab = tab;
    if (filter) state.filter = filter;
    state.threads = [];
    state.cursor = null;
    state.open.clear();
    $('feed').replaceChildren();
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
    $('show').replaceChildren(...show);
    const teams$ = v.teams.map((t) =>
      filterButton('@' + t, v.counts.teams[t] || 0, inThreads && f.team === t, () =>
        select('threads', { show: 'all', team: t })
      )
    );
    $('teams').replaceChildren(...(teams$.length ? teams$ : [el('div', 'none', 'No team')]));
    const people = v.people.map((h) =>
      filterButton('@' + h, undefined, inThreads && f.with === h, () =>
        select('threads', { show: 'all', with: h })
      )
    );
    $('people').replaceChildren(...(people.length ? people : [el('div', 'none', 'Nobody yet')]));
  }

  /// "@pedro → you", "@luis → @product", "@oscar → board".
  function target(t) {
    if (t.kind === 'global') return 'board';
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

  function fillConvo(box, t) {
    if (!box) return;
    if (!t) {
      box.replaceChildren(el('div', 'meta', 'Loading the conversation…'));
      return;
    }
    box.replaceChildren(
      ...t.notes.map((n) => {
        const row = el('div', 'note');
        const bubble = el('div', 'bubble');
        const meta = el('div', 'meta');
        meta.append(el('b', '', nameOf(n.author)), ' · ' + via(n) + ' · ');
        const time = el('span', '', relative(n.created_at, new Date()));
        time.title = absolute(n.created_at);
        meta.append(time);
        bubble.append(meta, renderNote(n.text, document));
        row.append(avatar(n.author), bubble);
        return row;
      })
    );
  }

  function post(t) {
    const a = el('article', 'post');
    a.dataset.id = t.id;
    a.tabIndex = 0;
    a.setAttribute('aria-expanded', state.open.has(t.id) ? 'true' : 'false');
    const first = t.first_note;
    const content = el('div', 'content');

    const head = el('div', 'head');
    head.append(el('b', '', nameOf(t.from)), el('span', 'h', '@' + t.from + ' → ' + target(t)));
    head.append(timeEl(first.created_at));
    content.append(head, el('div', 'via', 'via ' + via(first)));
    content.append(el('div', 'title', t.title));
    const rest = withoutTitle(first.text, t.title);
    if (rest.trim()) {
      content.append(renderNote(rest + (first.truncated ? '…' : ''), document, 'body clip'));
    }
    if (t.last_note) {
      const n = t.last_note;
      const reply = el('div', 'reply');
      const rh = el('div', 'head');
      rh.append(el('b', '', nameOf(n.author)), el('span', 'h', 'via ' + via(n)), timeEl(n.created_at));
      reply.append(rh, renderNote(n.text + (n.truncated ? '…' : ''), document, 'body clip'));
      content.append(reply);
    }
    const chipBox = el('div', 'chips');
    chips(t).forEach(([cls, text]) => chipBox.append(el('span', 'chip ' + cls, text)));
    content.append(chipBox);
    const open = state.open.has(t.id);
    if (t.notes > 2 || first.truncated || (t.last_note && t.last_note.truncated)) {
      const label = t.notes === 1 ? 'Read the whole note' : 'See conversation · ' + t.notes + ' notes';
      const more = el('div', 'more', open ? 'Hide conversation' : label);
      content.append(more);
    }
    if (open) {
      const box = el('div', 'convo');
      fillConvo(box, state.convos.get(t.id));
      content.append(box);
    }
    a.append(avatar(t.from), content);
    const toggle = () => {
      if (state.open.has(t.id)) state.open.delete(t.id);
      else {
        state.open.add(t.id);
        if (!state.convos.has(t.id)) loadConvo(t.id);
      }
      a.replaceWith(post(t));
      markCut($('feed'));
      const again = document.querySelector('[data-id="' + CSS.escape(t.id) + '"]');
      if (again) again.focus({ preventScroll: true });
    };
    a.onclick = (e) => {
      if (e.target.closest('.convo')) return;
      if (String(window.getSelection())) return;
      toggle();
    };
    a.onkeydown = (e) => {
      if ((e.key === 'Enter' || e.key === ' ') && e.target === a) {
        e.preventDefault();
        toggle();
      }
    };
    return a;
  }

  /// Previews cut by their height fade out at the bottom.
  function markCut(root) {
    root.querySelectorAll('.clip').forEach((c) => c.classList.toggle('cut', c.scrollHeight > c.clientHeight + 1));
  }

  function render() {
    $('tab-board').classList.toggle('on', state.tab === 'board');
    $('tab-threads').classList.toggle('on', state.tab === 'threads');
    $('tab-board').setAttribute('aria-selected', String(state.tab === 'board'));
    $('tab-threads').setAttribute('aria-selected', String(state.tab === 'threads'));
    renderSide();
    const feed = $('feed');
    feed.replaceChildren(...state.threads.map(post));
    markCut(feed);
    $('loadmore').hidden = !state.cursor;
    const footer = $('footer');
    if (!state.data) {
      footer.hidden = true;
    } else if (state.tab === 'board') {
      footer.textContent = state.threads.length
        ? 'This is the board: notes with no @, which everyone sees. Team notes and direct notes are seen only by the people in them.'
        : 'Nothing on the board yet. A note with no @ goes here, for everyone to see.';
      footer.hidden = !!state.cursor;
    } else {
      footer.textContent = state.threads.length ? '' : 'No threads here.';
      footer.hidden = !!state.cursor || !footer.textContent;
    }
  }

  $('tab-board').onclick = () => select('board');
  $('tab-threads').onclick = () => select('threads');
  $('refresh').onclick = () => refresh();
  $('loadmore').onclick = () => loadMore();
  setInterval(() => {
    if (document.visibilityState === 'visible') refresh();
  }, REFRESH_MS);
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'visible' && Date.now() - state.lastTry > REFRESH_MS) refresh();
  });
  // Relative times move on even when nothing new comes.
  setInterval(() => {
    if (document.visibilityState === 'visible' && !state.busy) render();
  }, 60000);
  refresh();
}
