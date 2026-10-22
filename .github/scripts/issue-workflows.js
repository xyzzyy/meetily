"use strict";

const { TEMPLATES, validate, newIssueUrl } = require("./issue-template.js");

const INVALID = "invalid-template";
const GRACE = "template-grace";
const AGAIN = "invalid-template-again";
const STATE_LABELS = {
  [INVALID]: { color: "d73a4a", description: "Required issue template information is missing" },
  [GRACE]: { color: "fbca04", description: "Edits are allowed until the next nightly template check at 04:00 UTC" },
  [AGAIN]: { color: "d4c5f9", description: "Further /reopen commands require a maintainer" },
};

function labelNames(issue) {
  return (issue.labels || []).map((label) => label.name || label);
}

function issueApi(context, github, issue) {
  const args = { ...context.repo, issue_number: issue.number };
  const labels = new Set(labelNames(issue));
  return {
    labels,
    get: async () => (await github.rest.issues.get(args)).data,
    update: async (changes) => github.rest.issues.update({ ...args, ...changes }),
    add: async (name) => {
      if (labels.has(name)) return;
      if (STATE_LABELS[name]) {
        try {
          await github.rest.issues.createLabel({ ...context.repo, name, ...STATE_LABELS[name] });
        } catch (error) {
          if (error.status !== 422) throw error;
        }
      }
      await github.rest.issues.addLabels({ ...args, labels: [name] });
      labels.add(name);
    },
    remove: async (name) => {
      if (!labels.has(name)) return;
      try {
        await github.rest.issues.removeLabel({ ...args, name });
      } catch (error) {
        if (error.status !== 404) throw error;
      }
      labels.delete(name);
    },
    comment: async (key, body) => {
      const marker = "<!-- meetily:" + key + " -->";
      const comments = await github.paginate(github.rest.issues.listComments, { ...args, per_page: 100 });
      if (comments.some((comment) => comment.user && comment.user.type === "Bot" && (comment.body || "").includes(marker))) return;
      await github.rest.issues.createComment({ ...args, body: marker + "\n" + body });
    },
  };
}

function problems(result, context) {
  if (!result.template) {
    const forms = Object.entries(TEMPLATES).map(([prefix, template]) =>
      "- `" + prefix.toUpperCase() + "`: [" + template.name + "](" + newIssueUrl(context.repo.owner, context.repo.repo, template) + ")"
    ).join("\n");
    return "The title must start with `[BUG]`, `[FEATURE]`, `[DOCS]`, or `[QUESTION]`. " +
      "Use the required sections from the matching form:\n\n" + forms;
  }
  const details = [];
  if (result.missing.length) {
    details.push("These required sections have no information:\n" +
      result.missing.map((heading) => "- `### " + heading + "`").join("\n"));
  }
  if (result.unchecked.length) {
    details.push("These required boxes are not checked:\n" +
      result.unchecked.map((item) => "- " + item).join("\n"));
  }
  return details.join("\n\n");
}

function reopenHelp(labels) {
  if (labels.has(AGAIN)) {
    return "This issue has `" + AGAIN + "`. Ask a maintainer to comment `/reopen` for another grace period.";
  }
  return "Anyone can comment `/reopen` once to start a grace period until the next nightly check at 04:00 UTC. " +
    "Further `/reopen` commands need a maintainer.";
}

// Event checks and nightly checks use the same rules and closure explanation.
// Only the nightly check can end an active grace period.
async function checkOpenIssue({ context, github, core }, issue, { nightly = false, dryRun = false } = {}) {
  if (issue.state !== "open" || issue.pull_request) return;
  const api = issueApi(context, github, issue);
  const result = validate(issue);
  if (api.labels.has(GRACE) && !nightly) {
    core.notice("#" + issue.number + " has grace until the next nightly check; edits remain allowed.");
    return;
  }

  const key = "template-check:" + context.runId + ":" + issue.number;
  if (result.ok) {
    const added = api.labels.has(result.template.label) ? [] : [result.template.label];
    const removed = [INVALID, GRACE].filter((name) => api.labels.has(name));
    if (!added.length && !removed.length) return;
    core.info("#" + issue.number + " follows its template; updating its labels.");
    if (dryRun) return;
    const changes = [];
    if (added.length) changes.push("I will add the `" + added[0] + "` label.");
    if (removed.length) changes.push("I will remove these labels: " + removed.map((name) => "`" + name + "`").join(", ") + ".");
    await api.comment(key, "This issue follows the [" + result.template.name + "](" +
      newIssueUrl(context.repo.owner, context.repo.repo, result.template) + ") template.\n\n" +
      changes.join("\n") + "\n\nThe issue will stay open.");
    for (const name of added) await api.add(name);
    for (const name of removed) await api.remove(name);
    return;
  }

  core.info("#" + issue.number + " does not follow its template; closing it.");
  if (dryRun) {
    core.info(problems(result, context));
    return;
  }
  const graceEnded = api.labels.has(GRACE) ? "The nightly check ends this issue's grace period.\n\n" : "";
  await api.comment(key, graceEnded + "I will close this issue because required template information is missing.\n\n" +
    problems(result, context) + "\n\nI will add `" + INVALID + "`. " +
    (api.labels.has(GRACE) ? "I will remove `" + GRACE + "`. " : "") +
    reopenHelp(api.labels));
  await api.add(INVALID);
  // Change labels while the issue is open. No cleanup runs on closed issues.
  await api.remove(GRACE);
  await api.update({ state: "closed", state_reason: "not_planned" });
}

async function enforce({ context, github, core }) {
  const issue = context.payload.issue;
  if (issue.state !== "open" || issue.pull_request) return;
  if (context.payload.action === "unlabeled" && (context.payload.label || {}).name !== GRACE) return;
  const live = await issueApi(context, github, issue).get();
  await checkOpenIssue({ context, github, core }, live);
}

async function reopen({ context, github, core }) {
  const { issue, comment } = context.payload;
  if (issue.pull_request || (comment.body || "").trim().toLowerCase() !== "/reopen") return;
  const live = await issueApi(context, github, issue).get();
  if (live.state !== "closed") return;
  const api = issueApi(context, github, live);
  const key = "reopen:" + comment.id;
  if (live.closed_at && comment.created_at && new Date(comment.created_at) < new Date(live.closed_at)) {
    await api.comment(key, "I did not reopen this issue because this command came before its latest closure. Submit a new `/reopen` request.");
    return;
  }
  if (!api.labels.has(INVALID)) {
    await api.comment(key, "I did not reopen this issue. `/reopen` applies only to issues closed for missing template information.");
    return;
  }

  if (api.labels.has(AGAIN)) {
    let maintainer = false;
    try {
      // Check the comment author, not an actor who reruns the workflow.
      const permission = (await github.rest.repos.getCollaboratorPermissionLevel({
        ...context.repo, username: comment.user.login,
      })).data;
      const roles = ["admin", "maintain", "write", "triage"];
      maintainer = roles.includes(permission.permission) || roles.includes(permission.role_name) ||
        !!(permission.user && permission.user.permissions && permission.user.permissions.triage);
    } catch (error) {
      await api.comment(key, "I could not check your maintainer permissions, so I did not reopen this issue. " +
        "It has `" + AGAIN + "`. A maintainer can try `/reopen` again when the permission check is available.");
      return;
    }
    if (!maintainer) {
      await api.comment(key, "I did not reopen this issue because it has `" + AGAIN + "`. " +
        "Ask a maintainer in this thread to comment `/reopen` for another grace period.");
      return;
    }
  }

  const hadGrace = api.labels.has(GRACE);
  const hadAgain = api.labels.has(AGAIN);
  await api.comment(key, "I will reopen this issue with `" + GRACE + "` until the next nightly check at 04:00 UTC.\n\n" +
    "You can make as many edits as needed while that label stays on the issue. " +
    "The nightly check will close it if required template information is still missing.\n\n" +
    "The `" + AGAIN + "` label means that further `/reopen` commands need a maintainer. " +
    "Maintainers can use the command without a limit.");
  try {
    // Set both labels before reopening, so an issue event sees the grace.
    // The labels record the allowance. No reopen count is needed.
    await api.add(GRACE);
    await api.add(AGAIN);
    await api.update({ state: "open" });
  } catch (error) {
    const remaining = [];
    for (const [name, existed] of [[GRACE, hadGrace], [AGAIN, hadAgain]]) {
      if (existed) continue;
      try {
        await api.remove(name);
      } catch (cleanupError) {
        core.warning("Could not undo the request label " + name + ": " + cleanupError.message);
        remaining.push(name);
      }
    }
    const explanation = remaining.length
      ? "I could not remove these request labels: " + remaining.map((name) => "`" + name + "`").join(", ") + ". Ask a maintainer to check them."
      : "I removed the labels added for this request. Please try `/reopen` again.";
    await api.comment(key + ":failed", "The reopen request failed. " + explanation);
    throw error;
  }
}

async function sweep({ context, github, core }) {
  const dryRun = context.eventName === "workflow_dispatch" && String((context.payload.inputs || {}).dry_run) === "true";
  const issues = await github.paginate(github.rest.issues.listForRepo, {
    ...context.repo, state: "open", per_page: 100,
  });
  if (dryRun) core.info("Dry run: no issue labels, comments, or states will change.");
  const failures = [];
  for (const issue of issues) {
    if (issue.state !== "open" || issue.pull_request) continue;
    try {
      // Refresh each open result. It may have changed or closed after listing.
      const live = await issueApi(context, github, issue).get();
      await checkOpenIssue({ context, github, core }, live, { nightly: true, dryRun });
    } catch (error) {
      core.warning("Could not check #" + issue.number + ": " + error.message);
      failures.push(issue.number);
    }
  }
  if (failures.length) throw new Error("The nightly check failed for issues: " + failures.join(", "));
}

module.exports = { enforce, reopen, sweep };
