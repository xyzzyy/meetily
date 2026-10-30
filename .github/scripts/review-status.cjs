async function reportIncompleteReview({ github, context }) {
  const pullRequest = context.payload.pull_request;
  if (!pullRequest) return;

  const runUrl = `${context.serverUrl || 'https://github.com'}/${context.repo.owner}/${context.repo.repo}/actions/runs/${context.runId}`;
  await github.rest.issues.createComment({
    ...context.repo,
    issue_number: pullRequest.number,
    body: [
      '<!-- meetily-opencode-review-incomplete -->',
      'The automated review did not finish. This run does not provide a complete review.',
      `Read the [workflow log](${runUrl}) for the failure or timeout. Any earlier findings still need review.`,
    ].join('\n\n'),
  });
}

module.exports = { reportIncompleteReview };
