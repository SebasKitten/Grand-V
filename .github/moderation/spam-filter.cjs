'use strict';

// Keep this narrow: mentioning PowerShell, performance or the campaign is not spam.
const blockedHost = /(?<![a-z0-9.-])(?:[a-z0-9-]+\.)*gitrm\.sbs(?![a-z0-9.-])/i;
const execution = /\b(?:iex|invoke-expression)\s*(?:\(|\s)[\s\S]*\b(?:irm|iwr|invoke-restmethod|invoke-webrequest)\b/i;
const promotion = /\b(?:Grand-VFullFix|quick\s+full\s+fix|optimizer\s*\+\s*crash\s+fix)\b/i;
const warningTitle = /^\s*(?:warning|psa|citizens\s+psa|do\s+not|don't|beware)\b/i;
const warningBody = /^\s*(?:warning|psa|do\s+not\s+(?:run|execute|paste)|don't\s+(?:run|execute|paste)|never\s+(?:run|execute|paste)|this\s+(?:is|looks\s+like)\s+(?:spam|malware))\b/i;
const notice = '[Maintainer spam filter]\n\nAn unauthorized GrandV remote-script advertisement was removed. This was not an official update or fix. Evidence was recorded in the Issue spam filter workflow log for maintainer review.';

function matchSpam({ title = '', body = '' }) {
  title = String(title).normalize('NFKC');
  body = String(body).normalize('NFKC');
  // Prefer a false negative over removing a community warning quoting the ad.
  if (warningTitle.test(title) || warningBody.test(body)) return null;
  const text = title + '\n' + body;
  return blockedHost.test(text) && execution.test(text) && promotion.test(text)
    ? 'known-remote-script-advertisement:gitrm.sbs' : null;
}

async function moderate({ github, context, core }) {
  const issue = context.payload.issue;
  if (!['issues', 'issue_comment'].includes(context.eventName) || !issue || issue.pull_request) {
    return { action: 'ignored-event' };
  }
  if (!Number.isSafeInteger(issue.number) || issue.number < 1) throw new Error('Invalid issue number');
  const { owner, repo } = context.repo;
  const isComment = context.eventName === 'issue_comment';
  const commentId = context.payload.comment?.id;
  if (isComment && (!Number.isSafeInteger(commentId) || commentId < 1)) throw new Error('Invalid comment ID');
  let target;
  try {
    // Read current content: an edited warning must not be replaced by an old event.
    target = (await (isComment
      ? github.rest.issues.getComment({ owner, repo, comment_id: commentId })
      : github.rest.issues.get({ owner, repo, issue_number: issue.number }))).data;
  } catch (error) {
    if (error.status === 404) return { action: 'already-removed' };
    throw error;
  }
  if (!isComment && target.pull_request) return { action: 'ignored-event' };
  if (target.user?.login?.toLowerCase() === owner.toLowerCase() ||
      ['OWNER', 'MEMBER', 'COLLABORATOR'].includes(target.author_association)) {
    return { action: 'trusted-author' };
  }
  const reason = matchSpam({ title: isComment ? '' : target.title, body: target.body });
  if (!reason) return { action: 'no-match' };
  const evidence = {
    schema: 1, recordedAtUtc: new Date().toISOString(), repository: owner + '/' + repo,
    issue: issue.number, commentId: isComment ? commentId : null,
    username: target.user?.login, accountUrl: target.user?.html_url,
    url: target.html_url, title: isComment ? null : target.title, body: target.body,
    createdAt: target.created_at, updatedAt: target.updated_at, reason,
    runId: context.runId,
  };
  // Encode BEFORE changing anything. Never print attacker text as workflow commands.
  core.info('SPAM_EVIDENCE_BASE64=' + Buffer.from(JSON.stringify(evidence), 'utf8').toString('base64'));
  if (isComment) {
    await github.rest.issues.updateComment({ owner, repo, comment_id: commentId, body: notice });
    // A spam comment must never close or lock an otherwise legitimate issue.
    core.info('SPAM_ACTION=comment-redacted');
    return { action: 'comment-redacted', username: evidence.username, url: evidence.url };
  }
  await github.rest.issues.update({ owner, repo, issue_number: issue.number,
    title: '[Spam removed] Unauthorized GrandV download advertisement',
    body: notice, state: 'closed', state_reason: 'not_planned' });
  if (!target.locked) await github.rest.issues.lock({ owner, repo, issue_number: issue.number, lock_reason: 'spam' });
  core.info('SPAM_ACTION=issue-redacted-closed-locked');
  return { action: 'issue-redacted-closed-locked', username: evidence.username, url: evidence.url };
}

module.exports = { matchSpam, moderate };
