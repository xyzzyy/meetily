"use strict";

const { TEMPLATES, validate, newIssueUrl } = require("./issue-template.js");
const { createHash } = require("node:crypto");

const INVALID = "invalid-template";
// Remove old recovery labels only when an open issue is checked.
const LEGACY_LABELS = ["template-grace", "invalid-template-again"];
const STATE_LABELS = {
  [INVALID]: { color: "d73a4a", description: "Required issue template information is missing" },
};

class IssueChanged extends Error {}

function labelNames(issue) {
  return (issue.labels || []).map((label) => label.name || label);
}

function issueApi(context, github, issue) {
  const args = { ...context.repo, issue_number: issue.number };
  const labels = new Set(labelNames(issue));
  async function beforeWrite() {
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
        // A successful check starts a new cycle, even for the same body.
        const latest = botComments.filter((comment) => /<!-- meetily:template-check:/.test(comment.body || "")).at(-1);
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

async function checkOpenIssue({ context, github, core }, issue) {
  if (issue.state !== "open" || issue.pull_request) return;
  const api = issueApi(context, github, issue);
  const result = validate(issue);

  const key = "template-check:" + context.runId + ":" + issue.number;
  if (result.ok) {
    const removed = [INVALID, ...LEGACY_LABELS].filter((name) => api.labels.has(name));
    // Restore the type only when clearing invalid-template. Legacy-label cleanup
    // must preserve a maintainer's decision to remove the type label.
    const added = removed.includes(INVALID) && !api.labels.has(result.template.label) ? [result.template.label] : [];
    if (!removed.length) return;
    core.info("#" + issue.number + " follows its template; updating its labels.");
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
  // Ignore labels changed by a partial closure, so another run can retry it
  // without repeating the same explanation.
  const stateKey = "invalid-state:" + createHash("sha256")
    .update(JSON.stringify([issue.title, issue.body])).digest("hex");
  await api.comment(key + ":" + stateKey, "I will close this issue because required template information is missing.\n\n" +
    problems(result, context) + "\n\nI will add `" + INVALID + "`. " +
    "I will remove the old recovery labels if they are present.\n\n" +
    "Edit this issue to add the missing information. Then ask a maintainer to reopen it, or open a new issue with the matching form. " +
    "Reopened issues have the same template check. The bot does not reopen issues from comments.", {
      stateKey,
      forceNew: context.payload.action === "reopened",
    });
  await api.add(INVALID);
  // Change labels while the issue is open. No cleanup runs on closed issues.
  for (const name of LEGACY_LABELS) await api.remove(name);
  await api.update({ state: "closed", state_reason: "not_planned" });
}

async function enforce({ context, github, core }) {
  if (context.eventName !== "issues" || !["opened", "reopened", "edited"].includes(context.payload.action)) return;
  const issue = context.payload.issue;
  if (issue.state !== "open" || issue.pull_request) return;
  const live = (await github.rest.issues.get({ ...context.repo, issue_number: issue.number })).data;
  try {
    await checkOpenIssue({ context, github, core }, live);
  } catch (error) {
    if (!(error instanceof IssueChanged)) throw error;
    core.notice(error.message);
  }
}

module.exports = { enforce };
