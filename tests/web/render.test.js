// The note renderer of relevo web (src/web/app.js), run with plain node by tests/web.rs:
// `node tests/web/render.test.js`. Note text is untrusted: it must come out as text nodes and a
// few allowed elements, never as HTML.
'use strict';
const assert = require('node:assert/strict');
const path = require('node:path');
const { parseNote, parseInline, toDom, preview, excerpt, withoutTitle, shownTitle, pickKey, askShown, short, noteTime, ago, absolute } = require(
  path.join(__dirname, '..', '..', 'src', 'web', 'app.js')
);

// A document that records what is built, and refuses every way of parsing HTML.
function fakeDocument() {
  const refuse = (what) => () => {
    throw new Error(what + ' used');
  };
  function node(tag) {
    const n = { tag, children: [], className: '', _text: null };
    Object.defineProperty(n, 'textContent', {
      set(v) {
        n.children = [];
        n._text = String(v);
      },
      get() {
        return n._text !== null ? n._text : n.children.map((c) => c.textContent).join('');
      },
    });
    for (const bad of ['innerHTML', 'outerHTML']) {
      Object.defineProperty(n, bad, { set: refuse(bad), get: refuse(bad) });
    }
    n.insertAdjacentHTML = refuse('insertAdjacentHTML');
    n.setAttribute = refuse('setAttribute');
    n.appendChild = (c) => {
      n._text = null;
      n.children.push(c);
      return c;
    };
    return n;
  }
  return {
    created: [],
    createElement(tag) {
      this.created.push(tag);
      return node(tag);
    },
    createTextNode(text) {
      const t = { tag: '#text', children: [] };
      Object.defineProperty(t, 'textContent', { get: () => text });
      return t;
    },
    createDocumentFragment() {
      return node('#fragment');
    },
    write: refuse('document.write'),
  };
}

// The tree as compact markup, to compare: text escaped so that it cannot pass for a tag.
function show(n) {
  if (n.tag === '#text') return n.textContent.replace(/&/g, '&amp;').replace(/</g, '&lt;');
  const inner = n._text !== null ? n._text.replace(/&/g, '&amp;').replace(/</g, '&lt;') : n.children.map(show).join('');
  if (n.tag === '#fragment') return inner;
  const cls = n.className ? '.' + n.className : '';
  return '<' + n.tag + cls + '>' + inner + '</' + n.tag + '>';
}

function render(text) {
  const doc = fakeDocument();
  return { html: show(toDom(parseNote(text), doc)), tags: doc.created };
}

const ALLOWED = new Set(['p', 'br', 'code', 'pre', 'strong', 'em', 'span']);
let count = 0;
function check(name, f) {
  f();
  count++;
}

check('script tags and HTML stay text', () => {
  const r = render('Hi <script>alert(1)</script> <img src=x onerror=alert(2)> &amp;');
  assert.equal(r.html, '<p>Hi &lt;script>alert(1)&lt;/script> &lt;img src=x onerror=alert(2)> &amp;amp;</p>');
  assert.deepEqual(r.tags, ['p']);
});

check('paragraphs and line breaks', () => {
  assert.equal(render('one\ntwo\n\n\nthree').html, '<p>one<br></br>two</p><p>three</p>');
  assert.equal(render('\n\n').html, '');
});

check('fenced code blocks, with a script inside', () => {
  const r = render('Look:\n```js\n<script>x()</script>\n  **not bold**\n```\nafter');
  assert.equal(
    r.html,
    '<p>Look:</p><pre><code>&lt;script>x()&lt;/script>\n  **not bold**</code></pre><p>after</p>'
  );
  // Tildes, longer fences, and an unclosed fence that runs to the end.
  assert.equal(render('~~~~\na\n~~~\nb\n~~~~').html, '<pre><code>a\n~~~\nb</code></pre>');
  assert.equal(render('```\n@ana <b>').html, '<pre><code>@ana &lt;b></code></pre>');
  // A fence closes only with the same character.
  assert.equal(render('```\na\n~~~\n```').html, '<pre><code>a\n~~~</code></pre>');
});

check('inline code, bold, italic', () => {
  assert.equal(render('use `a <b> c` now').html, '<p>use <code>a &lt;b> c</code> now</p>');
  assert.equal(render('``a ` b``').html, '<p><code>a ` b</code></p>');
  assert.equal(render('a ` lone').html, '<p>a ` lone</p>');
  assert.equal(render('**bold** and *it*').html, '<p><strong>bold</strong> and <em>it</em></p>');
  assert.equal(render('**bold *in* it**').html, '<p><strong>bold <em>in</em> it</strong></p>');
  assert.equal(render('2*3*4 and * not *').html, '<p>2*3*4 and * not *</p>');
  assert.equal(render('** no **').html, '<p>** no **</p>');
});

check('mentions are highlighted, as the server reads them', () => {
  assert.equal(render('@ana look').html, '<p><span.mention>@ana</span> look</p>');
  assert.equal(render('(@ana).').html, '<p>(<span.mention>@ana</span>).</p>');
  assert.equal(
    render('ask @ana/pagos-api@feat/refunds.').html,
    '<p>ask <span.mention>@ana/pagos-api@feat/refunds</span>.</p>'
  );
  // Emails, URLs, @@ and code mention nobody.
  for (const text of ['pedro@empresa.com', 'https://x.com/@ana', '@@ana', '`@ana`', '@a', '@ alone']) {
    assert.ok(!render(text).html.includes('mention'), text);
  }
});

check('links are text, never links', () => {
  const r = render('see https://evil.example/?a=<x> and [x](javascript:alert(1))');
  assert.equal(r.html, '<p>see https://evil.example/?a=&lt;x> and [x](javascript:alert(1))</p>');
  assert.ok(r.tags.every((t) => ALLOWED.has(t)));
});

check('only allowed elements, whatever the text', () => {
  const nasty = [
    '<iframe src=x>', '```\n</code></pre><script>', '**<b>**', '*<i>*', '`</code>`',
    '@<script>', '\u0000‮ text', '<!-- x -->', '<svg onload=alert(1)>',
  ];
  for (const text of nasty) {
    const r = render(text);
    assert.ok(r.tags.every((t) => ALLOWED.has(t)), text + ': ' + r.tags);
    assert.ok(!/<(script|iframe|svg|img|a)\b/.test(r.html), text + ': ' + r.html);
  }
  // A node of a kind it does not know is dropped, not built.
  const doc = fakeDocument();
  const frag = toDom([{ t: 'script', text: 'x' }, { t: 'p', children: [{ t: 'a', text: 'y' }] }], doc);
  assert.equal(show(frag), '<p></p>');
});

check('the title line is not shown twice', () => {
  assert.equal(withoutTitle('# Refunds\nbody', 'Refunds'), 'body');
  assert.equal(withoutTitle('\nRefunds\nbody', 'Refunds'), 'body');
  assert.equal(withoutTitle('Other\nbody', 'Refunds'), 'Other\nbody');
  // A first line over 80 characters: the server cut the title, the line starts with it.
  const long = '@oscar Https-callback-failure-in-refunds-service-production-eu-west-1-ProcessRefund-handler';
  const title = long.slice(0, 80);
  assert.equal(withoutTitle(long + '\nbody', title), 'body');
  assert.equal(withoutTitle('', ''), '');
  assert.equal(withoutTitle('body', ''), 'body');
});

check('titles: cut ones end in an ellipsis, leading mentions go', () => {
  const long = 'Https-callback-failure-in-refunds-service-production-eu-west-1-ProcessRefund-handler-x';
  assert.equal(shownTitle(long.slice(0, 80), long), long.slice(0, 80) + '…');
  assert.equal(shownTitle('Refunds', '# Refunds\nbody'), 'Refunds');
  assert.equal(shownTitle('@oscar can you review it?', '@oscar can you review it?'), 'Can you review it?');
  assert.equal(shownTitle('@product, @ana/pagos: who owns it?', '@product, @ana/pagos: who owns it?'), 'Who owns it?');
  assert.equal(shownTitle('@ana', '@ana'), '@ana');
  assert.equal(shownTitle('ana@example.com is down', 'ana@example.com is down'), 'ana@example.com is down');
});

check('the key: from the address, else what the tab kept', () => {
  assert.equal(pickKey('#k=abc', 'old'), 'abc');
  assert.equal(pickKey('', 'old'), 'old');
  assert.equal(pickKey('#x=1', ''), '');
  assert.equal(pickKey(undefined, undefined), '');
});

check('absolute times are in English', () => {
  const t = absolute(new Date(2026, 10, 11, 15, 31).toISOString());
  assert.match(t, /^Wednesday,? 11 November 2026/);
  assert.match(t, /15:31/);
});

check('a row\'s times are short', () => {
  const now = new Date(2026, 10, 11, 15, 0, 0);
  const ago_ = (mins) => new Date(now.getTime() - mins * 60000).toISOString();
  assert.equal(short(ago_(0.5), now), 'now');
  assert.equal(short(ago_(15), now), '15m');
  assert.equal(short(ago_(180), now), '3h');
  assert.equal(short(new Date(2026, 10, 10, 9, 0).toISOString(), now), 'Tue');
  assert.equal(short(new Date(2026, 10, 5, 9, 0).toISOString(), now), 'Thu');
  assert.equal(short(new Date(2026, 10, 4, 9, 0).toISOString(), now), '4 Nov');
  assert.equal(short(new Date(2025, 9, 3, 9, 0).toISOString(), now), '3 Oct 2025');
});

check('a note\'s time in a conversation, and the foot\'s', () => {
  const now = new Date(2026, 10, 11, 17, 0, 0);
  assert.equal(noteTime(new Date(2026, 10, 11, 16, 32).toISOString(), now), '16:32');
  assert.equal(noteTime(new Date(2026, 10, 10, 9, 5).toISOString(), now), 'Tue 09:05');
  assert.equal(noteTime(new Date(2026, 10, 1, 9, 5).toISOString(), now), '1 Nov 09:05');
  assert.equal(ago(new Date(now.getTime() - 20000), now), 'just now');
  assert.equal(ago(new Date(now.getTime() - 125000), now), '2 min ago');
  assert.equal(ago(new Date(now.getTime() - 3 * 3600000), now), '3 h ago');
});

check('a row\'s preview is one paragraph, only allowed elements', () => {
  const doc = fakeDocument();
  const text = 'Here is the **snippet** <img src=x>\nnext line:\n```js\nconst x = 1;\nmore\n```\nInline `<b>`';
  assert.equal(
    show(toDom(preview(text), doc)),
    '<p>Here is the <strong>snippet</strong> &lt;img src=x> next line: <code>const x = 1;…</code> Inline <code>&lt;b></code></p>'
  );
  assert.ok(doc.created.every((t) => ALLOWED.has(t)));
  assert.deepEqual(preview(''), []);
  // The first note after its title, without the @mentions it starts with.
  assert.equal(excerpt('@oscar Release train\nWhen does it ship?', '@oscar Release train'), 'When does it ship?');
  assert.equal(excerpt('Title\n@ana @bob: look', 'Title'), 'look');
  assert.equal(excerpt('Title only', 'Title only'), '');
});

check('parseInline keeps line breaks inside a paragraph', () => {
  assert.deepEqual(parseInline('a\nb'), [{ t: 'text', text: 'a' }, { t: 'br' }, { t: 'text', text: 'b' }]);
});

// `check` is synchronous: these run after it, and fail the process when they fail.
const pending = [];
function checkAsync(name, f) {
  pending.push(
    f().then(
      () => count++,
      (e) => {
        console.error('FAILED: ' + name);
        throw e;
      }
    )
  );
}

// Threads with cursors 60 (newest) down to 1, as the server pages them.
function server(cursors) {
  const asked = [];
  const ask = async (before, limit) => {
    asked.push([before, limit]);
    const all = cursors.filter((c) => before === null || c < Number(before));
    const page = all.slice(0, limit);
    const more = all.length > limit;
    return {
      threads: page.map((c) => ({ id: 't' + c, cursor: String(c) })),
      cursor: more ? String(page[page.length - 1]) : null,
    };
  };
  return { ask, asked };
}
const sixty = Array.from({ length: 60 }, (_, i) => 60 - i);

checkAsync('a refresh asks again every page shown, past the first 50', async () => {
  // Nothing shown: the first page.
  let s = server(sixty);
  let got = await askShown(s.ask, 0, null);
  assert.deepEqual(s.asked, [[null, 20]]);
  assert.equal(got.threads.length, 20);
  assert.equal(got.cursor, '41');
  // 60 shown (Load more until thread 1, cursor null at the end): every page, to the end.
  s = server(sixty);
  got = await askShown(s.ask, 60, null);
  assert.deepEqual(s.asked, [[null, 50], ['11', 50]]);
  assert.equal(got.threads.length, 60);
  assert.equal(got.cursor, null);
  // 55 shown, down to cursor 6: down to it, not further than a page past it.
  s = server(sixty);
  got = await askShown(s.ask, 55, '6');
  assert.deepEqual(s.asked, [[null, 50], ['11', 50]]);
  assert.equal(got.cursor, null);
  // 40 shown, down to 21: one page of 40 is enough.
  s = server(sixty);
  got = await askShown(s.ask, 40, '21');
  assert.deepEqual(s.asked, [[null, 40]]);
  assert.equal(got.cursor, '21');
  // Thread 55 was closed far down without a note: it is in the answer as it is now.
  assert.ok(got.threads.some((t) => t.id === 't55'));
  // An answer no longer wanted stops it.
  got = await askShown(async () => null, 60, null);
  assert.equal(got, null);
});

Promise.all(pending).then(
  () => console.log('renderer: ' + count + ' checks passed'),
  (e) => {
    console.error(e);
    process.exit(1);
  }
);
