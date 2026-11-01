"use strict";

const { TEMPLATES, validate, newIssueUrl } = require("./issue-template.js");
const { createHash } = require("node:crypto");

const INVALID = "invalid-template";
const GRACE = "template-grace";
const AGAIN = "invalid-template-again";
const STATE_LABELS = {
  [INVALID]: { color: "d73a4a", description: "Required issue template information is missing" },
  [AGAIN]: { color: "d4c5f9", description: "Required template information is still missing after a grace period" },
  [GRACE]: { color: "fbca04", description: "Edits are allowed until the next nightly template check at 04:00 UTC" },
};

class IssueChanged extends Error {}

function labelNames(issue) {
  return (issue.labels || []).map((label) => label.name || label);
}

function issueApi(context, github, issue, requireCurrent = false) {
  const args = { ...context.repo, issue_number: issue.number };
  const labels = new Set(labelNames(issue));
  async function beforeWrite() {
    if (!requireCurrent) return;
    const live = (await github.rest.issues.get(args)).data;
    // Check state first. Never inspect the content of an issue now closed.
    // Our comments do not affect these fields; our label writes update labels.
    if (live.state !== "open" || live.title !== issue.title || live.body !== issue.body ||
        JSON.stringify(labelNames(live).sort()) !== JSON.stringify([...labels].sort())) {
      throw new IssueChanged("#" + issue.number + " changed during this check; stopping this pass.");
    }
  }
  return {
    labels,
    get: async () => (await github.rest.issues.get(args)).data,
    update: async (changes) => {
      await beforeWrite();
      return github.rest.issues.update({ ...args, ...changes });
    },
    add: async (name) => {
      if (labels.has(name)) return;
      if (STATE_LABELS[name]) {
        await beforeWrite();
        try {
          await github.rest.issues.createLabel({ ...context.repo, name, ...STATE_LABELS[name] });
        } catch (error) {
          if (error.status !== 422) throw error;
        }
      }
      await beforeWrite();
      await github.rest.issues.addLabels({ ...args, labels: [name] });
      labels.add(name);
    },
    remove: async (name) => {
      if (!labels.has(name)) return;
      await beforeWrite();
      try {
        await github.rest.issues.removeLabel({ ...args, name });
      } catch (error) {
        if (error.status !== 404) throw error;
      }
      labels.delete(name);
    },
    comment: async (key, body, { stateKey, forceNew = false } = {}) => {
      const marker = "<!-- meetily:" + key + " -->";
      const comments = await github.paginate(github.rest.issues.listComments, { ...args, per_page: 100 });
      const botComments = comments.filter((comment) => comment.user && comment.user.type === "Bot");
      if (botComments.some((comment) => (comment.body || "").includes(marker))) return;
      const stateMarker = stateKey ? "<!-- meetily:" + stateKey + " -->\n" : "";
      if (stateKey && !forceNew) {
        // Reuse an explanation only while it is the latest template action.
        // A reopen or successful check starts a new cycle, even for the same body.
        const latest = botComments.filter((comment) => /<!-- meetily:(?:template-check:|reopen:)/.test(comment.body || "")).at(-1);
        if (latest && (latest.body || "").includes(stateMarker.trim())) return;
      }
      await beforeWrite();
      await github.rest.issues.createComment({ ...args, body: marker + "\n" + stateMarker + body });
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
// A complete correction ends grace; the nightly check closes incomplete issues.
async function checkOpenIssue({ context, github, core }, issue, { nightly = false, dryRun = false } = {}) {
  if (issue.state !== "open" || issue.pull_request) return;
  const api = issueApi(context, github, issue, true);
  const result = validate(issue);
  if (api.labels.has(GRACE) && !nightly && !result.ok) {
    // Retained grace after a manual close and reopen is allowed intentionally.
    const stale = [INVALID, AGAIN].filter(name => api.labels.has(name));
    if (stale.length) {
      await api.comment("grace-labels:" + context.runId + ":" + issue.number,
        "This issue is in its grace period. I will keep `" + GRACE +
        "` and remove the other template labels. You can continue editing it.");
      for (const name of stale) await api.remove(name);
    }
    core.notice("#" + issue.number + " has grace until the next nightly check; edits remain allowed.");
    return;
  }

  const key = "template-check:" + context.runId + ":" + issue.number;
  if (result.ok) {
    const removed = [INVALID, GRACE, AGAIN].filter((name) => api.labels.has(name));
    // The forms label new issues. Add the type label only when recovering an
    // issue, so a maintainer who removes it during triage is not overridden.
    const added = (api.labels.has(INVALID) || api.labels.has(GRACE)) && !api.labels.has(result.template.label) ? [result.template.label] : [];
    if (!removed.length) return;
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
  const invalidLabel = api.labels.has(GRACE) || api.labels.has(AGAIN) ? AGAIN : INVALID;
  const graceEnded = api.labels.has(GRACE) ? "The nightly check ends this issue's grace period.\n\n" : "";
  // Ignore labels changed by a partial closure, so another run can retry it
  // without repeating the same explanation. The repeat allowance changes help text.
  const stateKey = "invalid-state:" + createHash("sha256")
    .update(JSON.stringify([issue.title, issue.body, invalidLabel])).digest("hex");
  await api.comment(key + ":" + stateKey, graceEnded + "I will close this issue because required template information is missing.\n\n" +
    problems(result, context) + "\n\nI will add `" + invalidLabel + "`. " +
    "I will remove the other template labels. " + reopenHelp(new Set([invalidLabel])), {
      stateKey,
      forceNew: context.eventName === "issues" && context.payload.action === "reopened",
    });
  await api.add(invalidLabel);
  // Change labels while the issue is open. No cleanup runs on closed issues.
  for (const name of [INVALID, GRACE, AGAIN]) {
    if (name !== invalidLabel) await api.remove(name);
  }
  await api.update({ state: "closed", state_reason: "not_planned" });
}

async function enforce({ context, github, core }) {
  const issue = context.payload.issue;
  if (issue.state !== "open" || issue.pull_request) return;
  if (context.payload.action === "unlabeled" && (context.payload.label || {}).name !== GRACE) return;
  const live = await issueApi(context, github, issue).get();
  try {
    await checkOpenIssue({ context, github, core }, live);
  } catch (error) {
    if (!(error instanceof IssueChanged)) throw error;
    core.notice(error.message);
  }
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
  if (!api.labels.has(INVALID) && !api.labels.has(AGAIN) && !api.labels.has(GRACE)) {
    await api.comment(key, "I did not reopen this issue. `/reopen` applies only to issues closed for missing template information.");
    return;
  }

  if (api.labels.has(AGAIN) || api.labels.has(GRACE)) {
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
        "A maintainer can try `/reopen` again when the permission check is available.");
      return;
    }
    if (!maintainer) {
      await api.comment(key, "I did not reopen this issue because it has already had a grace period. " +
        "Ask a maintainer in this thread to comment `/reopen` for another grace period.");
      return;
    }
  }

  const hadGrace = api.labels.has(GRACE);
  await api.comment(key, "I will reopen this issue with `" + GRACE + "` until the next nightly check at 04:00 UTC, or until you complete the template.\n\n" +
    "I will remove `" + INVALID + "` and `" + AGAIN + "` if present. " +
    "You can make as many edits as needed while that label stays on the issue. " +
    "The nightly check will close it if required template information is still missing.\n\n" +
    "If the nightly check closes it again, it will have only `" + AGAIN + "`. Further `/reopen` commands then need a maintainer. " +
    "Maintainers can use the command without a limit.");
  try {
    // Set grace before reopening, so the issue event allows edits.
    // Clear the closed-state labels only after reopening succeeds.
    await api.add(GRACE);
    await api.update({ state: "open" });
    await api.remove(INVALID);
    await api.remove(AGAIN);
  } catch (error) {
    // A failed response does not prove that GitHub rejected the update.
    // Read the actual state before removing any request labels.
    let current;
    try {
      current = await api.get();
    } catch (stateError) {
      core.warning("Could not confirm the reopen state: " + stateError.message);
      await api.comment(key + ":uncertain", "The API response failed, and I could not confirm the issue's current state. " +
        "I kept the request labels so an open issue keeps its grace. Ask a maintainer to check the issue before retrying.");
      throw error;
    }
    const recovery = issueApi(context, github, current);
    if (current.state === "open") {
      await recovery.add(GRACE);
      await recovery.remove(INVALID);
      await recovery.remove(AGAIN);
      await recovery.comment(key + ":confirmed", "The API response failed, but I confirmed that this issue is open. " +
        "I kept only `" + GRACE + "` of the template labels. The grace period remains active until the nightly check.");
      return;
    }
    if (current.closed_at && live.closed_at && current.closed_at !== live.closed_at) {
      await recovery.comment(key + ":closed-again", "The API response failed, and this issue has a later closure. " +
        "I kept its request labels and left it closed.");
      return;
    }
    const remaining = [];
    for (const [name, existed] of [[GRACE, hadGrace]]) {
      if (existed) continue;
      try {
        await recovery.remove(name);
      } catch (cleanupError) {
        core.warning("Could not undo the request label " + name + ": " + cleanupError.message);
        remaining.push(name);
      }
    }
    const explanation = remaining.length
      ? "I could not remove these request labels: " + remaining.map((name) => "`" + name + "`").join(", ") + ". Ask a maintainer to check them."
      : "I removed the labels added for this request. Please try `/reopen` again.";
    await recovery.comment(key + ":failed", "The reopen request failed. " + explanation);
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
      if (error instanceof IssueChanged) {
        core.notice(error.message);
        continue;
      }
      core.warning("Could not check #" + issue.number + ": " + error.message);
      const headers = (error.response || {}).headers || {};
      if (error.status === 429 || (error.status === 403 &&
          (String(headers["x-ratelimit-remaining"]) === "0" || headers["retry-after"] !== undefined ||
           /rate limit|abuse detection/i.test(error.message || "")))) {
        throw new Error("The nightly check stopped at #" + issue.number +
          " after a GitHub rate limit. Remaining issues were not checked.", { cause: error });
      }
      failures.push(issue.number);
    }
  }
  if (failures.length) throw new Error("The nightly check failed for issues: " + failures.join(", "));
}

module.exports = { enforce, reopen, sweep };
