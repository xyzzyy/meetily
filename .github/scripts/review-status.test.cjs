const { test } = require('node:test');
const assert = require('node:assert/strict');
const { reportIncompleteReview } = require('./review-status.cjs');

test('posts an incomplete status on the PR with the failed run link', async () => {
  const calls = [];
  await reportIncompleteReview({
    github: { rest: { issues: { createComment: async request => calls.push(request) } } },
    context: {
      payload: { pull_request: { number: 30 } },
      repo: { owner: 'owner', repo: 'repo' }, runId: 123, serverUrl: 'https://github.example',
    },
  });
  assert.equal(calls.length, 1);
  assert.equal(calls[0].issue_number, 30);
  assert.equal(calls[0].owner, 'owner');
  assert.equal(calls[0].repo, 'repo');
  assert.match(calls[0].body, /does not provide a complete review/);
  assert.match(calls[0].body, /https:\/\/github.example\/owner\/repo\/actions\/runs\/123/);
});

test('does not post when the event has no PR', async () => {
  await reportIncompleteReview({ github: null, context: { payload: {} } });
});

test('reports an API failure so the workflow cannot claim the notice was posted', async () => {
  await assert.rejects(reportIncompleteReview({
    github: { rest: { issues: { createComment: async () => { throw new Error('API unavailable'); } } } },
    context: { payload: { pull_request: { number: 30 } }, repo: { owner: 'owner', repo: 'repo' }, runId: 123 },
  }), /API unavailable/);
});
