# Issue spam filter

Runs when issues or issue comments are created or edited, and when issues reopen.
It requires the known `gitrm.sbs` host, a PowerShell download-and-execute pattern,
and the campaign's fix-advertising wording together. Broad words such as
"PowerShell", "optimizer", or the spam title alone do not trigger moderation.
Explicit warning posts and repository owners/collaborators/members are exempt.
This conservative rule is specific to the observed campaign, not a malware scanner.

Matched issues have their advertisement replaced by a maintainer notice and are
closed and locked. Matched comments are redacted without closing their parent
issue. There are no permanent deletions, account-wide blocks, interaction limits,
custom secrets, or changes to game/launcher code in this workflow.

Before moderation, the full original content, username, account/post URLs, time,
reason, and run ID are written to the Actions log as `SPAM_EVIDENCE_BASE64=...`.
Decode that value as base64 UTF-8 JSON for the campaign evidence log; never execute
the original content. `SPAM_ACTION=...` records completed moderation. The existing
monitor can collect these records, block authors, and maintain the report queue.

The workflow reads moderation code from `main`, uses pinned official actions,
does not execute issue text, and has only `contents: read` and `issues: write`.
Use **Actions → Issue spam filter → Run workflow** for a non-mutating self-test.
To disable it, use the workflow's **Disable workflow** option.

Local tests: `node --test .github/moderation/spam-filter.test.cjs`.
