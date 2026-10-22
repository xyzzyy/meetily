const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");

const root = path.resolve(__dirname, "../..");
process.env.GITHUB_WORKSPACE = root;
const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;

// Run the actual inline scripts and job guards, with no YAML dependency.
// This reader only supports the literal scripts and folded guards used here.
function loadJob(file, job) {
  const workflow = fs.readFileSync(path.join(root, ".github/workflows", file), "utf8");
  const block = workflow.match(new RegExp(`^  ${job}:\\n([\\s\\S]*?)(?=^  [\\w-]+:|$(?![\\s\\S]))`, "m"));
  assert.ok(block, `Missing job: ${job}`);
  const lines = block[1].split("\n");
  const start = lines.findIndex((line) => line.trim() === "script: |");
  assert.notEqual(start, -1, `Missing script: ${job}`);
  const indent = lines[start].match(/^ */)[0].length + 2;
  const script = [];
  for (const line of lines.slice(start + 1)) {
    if (line.trim() && !line.startsWith(" ".repeat(indent))) break;
    script.push(line.slice(indent));
  }
  const run = new AsyncFunction("context", "github", "core", "require", script.join("\n"));
  const guardIndex = lines.findIndex((line) => line.startsWith("    if:"));
  assert.notEqual(guardIndex, -1, `Missing job guard: ${job}`);
  let guard = lines[guardIndex].slice("    if:".length).trim();
  if (guard === ">-") {
    const parts = [];
    for (const line of lines.slice(guardIndex + 1)) {
      if (!line.startsWith("      ")) break;
      parts.push(line.trim());
    }
    guard = parts.join(" ");
  }
  const allowed = new Function("github", `return (${guard});`);
  return async (fixture) => {
    if (!allowed({ event_name: fixture.context.eventName, event: fixture.context.payload })) return;
    await run(fixture.context, fixture.github, fixture.core, require);
  };
}

const enforce = loadJob("issue-enforce.yml", "enforce");
const reopen = loadJob("issue-grace.yml", "reopen");
const expire = loadJob("issue-grace.yml", "expire");
const cleanup = loadJob("issue-grace.yml", "cleanup");

const completeBody = [
  "### Bug Description", "The app stops.",
  "### Current Behavior", "The app does not open.",
  "### Expected Behavior", "The app opens.",
  "### Steps to Reproduce", "Start the app.",
  "### Environment", "macOS 15",
  "### Checklist",
  "- [x] I have described the issue clearly",
  "- [x] I have searched for similar issues",
].join("\n");

function fixture(options = {}) {
  const issue = {
    number: 1,
    title: "[BUG] The app stops",
    body: "### Bug Description\nThe app stops.",
    state: "open",
    user: { login: "author" },
    author_association: "NONE",
    labels: [],
    ...options.issue,
  };
  const calls = [];
  const comments = (options.comments || []).map((body, id) => ({ id, body }));
  const payloadIssue = structuredClone(issue);
  const context = {
    repo: { owner: "owner", repo: "repo" },
    actor: "author",
    eventName: options.eventName || "issues",
    payload: {
      action: options.action || "opened",
      issue: payloadIssue,
      comment: { id: 100, body: "/reopen", user: { login: options.commenter || "author" } },
      inputs: { dry_run: "false" },
    },
  };
  Object.assign(issue, options.live);
  const labels = () => issue.labels.map((label) => label.name || label);
  const failure = (message) => Object.assign(new Error(message), { status: 500 });
  const github = {
    rest: {
      issues: {
        get: async (args) => {
          calls.push({ method: "get", ...args });
          return { data: structuredClone(issue) };
        },
        listForRepo: async () => ({ data: issue.state === "open" && labels().includes("template-grace") ? [structuredClone(issue)] : [] }),
        listComments: async () => ({ data: structuredClone(comments) }),
        createComment: async ({ body }) => {
          calls.push({ method: "createComment", body });
          comments.push({ id: 200 + comments.length, body });
        },
        addLabels: async ({ labels: added }) => {
          calls.push({ method: "addLabels", labels: added });
          if (options.failLabel && added.includes(options.failLabel)) throw failure("Label failed");
          for (const label of added) if (!labels().includes(label)) issue.labels.push({ name: label });
        },
        removeLabel: async ({ name }) => {
          calls.push({ method: "removeLabel", name });
          issue.labels = issue.labels.filter((label) => (label.name || label) !== name);
        },
        createLabel: async (args) => { calls.push({ method: "createLabel", ...args }); },
        update: async (args) => {
          calls.push({ method: "update", labels: labels(), ...args });
          if (options.failUpdate) throw failure("Update failed");
          if (args.state) issue.state = args.state;
        },
      },
      repos: {
        getCollaboratorPermissionLevel: async (args) => {
          calls.push({ method: "permission", ...args });
          if (options.failPermission) throw failure("Permission lookup failed");
          return { data: options.permission || { permission: "read", role_name: "read" } };
        },
      },
    },
    paginate: async (method, args) => (await method(args)).data,
  };
  const core = {
    notice: () => {},
    info: () => {},
    warning: (message) => { calls.push({ method: "warning", message }); },
  };
  return { context, github, core, issue, calls, comments, labels };
}

function closedFixture(options = {}) {
  return fixture({
    ...options,
    eventName: "issue_comment",
    issue: { state: "closed", labels: ["invalid-template"], ...options.issue },
  });
}

function graceFixture(options = {}) {
  return fixture({
    ...options,
    eventName: "schedule",
    issue: { labels: ["invalid-template", "template-grace", "template-grace-used"], ...options.issue },
  });
}

for (const association of ["NONE", "OWNER", "MEMBER", "COLLABORATOR"]) {
  for (const action of ["opened", "edited", "reopened"]) {
    test(`${association} issue: ${action} enforces the template`, async () => {
      const f = fixture({ issue: { author_association: association }, action });
      f.context.actor = "maintainer";
      await enforce(f);
      assert.equal(f.issue.state, "closed");
      assert.ok(f.labels().includes("invalid-template"));
      assert.equal(f.comments.length, 1);
      assert.match(f.comments[0].body, /\/reopen/);
      assert.doesNotMatch(f.comments[0].body, /exempt/);
      assert.equal(f.calls.filter((call) => call.method === "permission").length, 0);
    });
  }
}

test("a complete maintainer issue passes", async () => {
  const f = fixture({ issue: { author_association: "OWNER", body: completeBody } });
  await enforce(f);
  assert.equal(f.issue.state, "open");
  assert.ok(f.labels().includes("bug"));
});

test("enforcement uses the current body and state", async () => {
  const fixed = fixture({ live: { body: completeBody } });
  await enforce(fixed);
  assert.equal(fixed.issue.state, "open");
  assert.ok(fixed.labels().includes("bug"));
  const closed = fixture({ live: { state: "closed" } });
  await enforce(closed);
  assert.equal(closed.comments.length, 0);
});

test("an edit to a closed issue does not start enforcement", async () => {
  const f = fixture({ issue: { state: "closed" }, action: "edited" });
  await enforce(f);
  assert.deepEqual(f.calls, []);
});

test("a manual maintainer reopen has no exemption or extra grace", async () => {
  const f = fixture({ action: "reopened", issue: { labels: ["invalid-template", "template-grace-used"] } });
  f.context.actor = "maintainer";
  await enforce(f);
  assert.equal(f.issue.state, "closed");
  assert.ok(!f.labels().includes("template-grace"));
});

test("an incomplete grace issue stays open without repeated guidance", async () => {
  const f = graceFixture({ issue: { author_association: "OWNER" } });
  await enforce(f);
  assert.equal(f.issue.state, "open");
  assert.equal(f.comments.length, 0);
});

test("a fixed grace issue clears active labels and keeps the used chance", async () => {
  const f = graceFixture({ issue: { body: completeBody, author_association: "OWNER" } });
  await enforce(f);
  assert.deepEqual(f.labels().sort(), ["bug", "template-grace-used"]);
});

test("the author gets one incomplete reopen with labels set before the state change", async () => {
  const f = closedFixture();
  await reopen(f);
  assert.equal(f.issue.state, "open");
  const update = f.calls.find((call) => call.method === "update");
  assert.ok(update.labels.includes("template-grace"));
  assert.ok(update.labels.includes("template-grace-used"));
  assert.match(f.comments[0].body, /04:00 UTC/);
  // Even if an issue event is delivered, enforcement respects the same window.
  f.context.payload.issue.state = "open";
  await enforce(f);
  assert.equal(f.issue.state, "open");
});

test("a fixed first reopen does not use the author's grace chance", async () => {
  const f = closedFixture({ issue: { body: completeBody } });
  await reopen(f);
  assert.equal(f.issue.state, "open");
  assert.deepEqual(f.labels(), ["bug"]);
});

test("an author cannot grant a second reopen, even after a fix", async () => {
  for (const body of ["incomplete", completeBody]) {
    const f = closedFixture({ issue: { body, labels: ["invalid-template", "template-grace-used"] } });
    await reopen(f);
    await reopen(f);
    assert.equal(f.issue.state, "closed");
    assert.equal(f.comments.length, 1);
    assert.match(f.comments[0].body, /maintainer can comment `\/reopen`/i);
  }
});

for (const permission of [
  { permission: "admin", role_name: "admin" },
  { permission: "write", role_name: "write" },
  { permission: "write", role_name: "maintain" },
  { permission: "read", role_name: "triage" },
  { permission: "read", role_name: "custom-triage", user: { permissions: { triage: true } } },
]) {
  for (const commenter of ["author", "maintainer"]) {
    test(`${permission.role_name} can grant more cycles as ${commenter}`, async () => {
      const f = closedFixture({ commenter, permission, issue: { labels: ["invalid-template", "template-grace-used"] } });
      await reopen(f);
      assert.equal(f.issue.state, "open");
      assert.ok(f.labels().includes("template-grace"));
      assert.ok(f.labels().includes("template-grace-used"));
      assert.match(f.comments[0].body, /04:00 UTC/);
    });
  }
}

test("a maintainer can reopen a fixed issue after the author used the chance", async () => {
  const f = closedFixture({ commenter: "maintainer", permission: { permission: "write" }, issue: { body: completeBody, labels: ["invalid-template", "template-grace-used"] } });
  await reopen(f);
  assert.equal(f.issue.state, "open");
  assert.deepEqual(f.labels().sort(), ["bug", "template-grace-used"]);
});

test("read access and author association cannot grant another person's reopen", async () => {
  const f = closedFixture({ commenter: "reader" });
  f.context.payload.comment.author_association = "MEMBER";
  await reopen(f);
  assert.equal(f.issue.state, "closed");
  assert.equal(f.comments.length, 0);
});

test("the permission lookup uses the comment author, not a maintainer who reruns the job", async () => {
  const f = closedFixture({ commenter: "reader" });
  f.context.actor = "maintainer";
  await reopen(f);
  assert.equal(f.issue.state, "closed");
  assert.equal(f.calls.find((call) => call.method === "permission").username, "reader");
});

test("permission lookup failure never grants extra access", async () => {
  const author = closedFixture({ failPermission: true });
  await reopen(author);
  assert.equal(author.issue.state, "open");
  const usedAuthor = closedFixture({ failPermission: true, issue: { labels: ["invalid-template", "template-grace-used"] } });
  await reopen(usedAuthor);
  assert.equal(usedAuthor.issue.state, "closed");
  const other = closedFixture({ failPermission: true, commenter: "maintainer" });
  await reopen(other);
  assert.equal(other.issue.state, "closed");
});

test("a failed first reopen does not use the author's chance", async () => {
  const f = closedFixture({ failUpdate: true });
  await assert.rejects(reopen(f), /Update failed/);
  assert.deepEqual(f.labels(), ["invalid-template"]);
});

test("a failed maintainer reopen keeps the author's previously used chance", async () => {
  const f = closedFixture({ failUpdate: true, commenter: "maintainer", permission: { permission: "write" }, issue: { labels: ["invalid-template", "template-grace-used"] } });
  await assert.rejects(reopen(f), /Update failed/);
  assert.deepEqual(f.labels().sort(), ["invalid-template", "template-grace-used"]);
});

test("a failed label addition removes the new active window", async () => {
  const f = closedFixture({ failLabel: "template-grace-used" });
  await assert.rejects(reopen(f), /Label failed/);
  assert.equal(f.issue.state, "closed");
  assert.deepEqual(f.labels(), ["invalid-template"]);
});

test("/reopen ignores open issues, pull requests, ordinary comments, and other closure labels", async () => {
  const open = closedFixture({ issue: { state: "open" } });
  const pr = closedFixture({ issue: { pull_request: {} } });
  const ordinary = closedFixture();
  ordinary.context.payload.comment.body = "Please reopen this";
  const duplicate = closedFixture({ commenter: "maintainer", permission: { permission: "admin" }, issue: { labels: ["duplicate"] } });
  for (const f of [open, pr, ordinary, duplicate]) {
    await reopen(f);
    assert.equal(f.calls.filter((call) => call.method === "update").length, 0);
    assert.equal(f.comments.length, 0);
  }
});

test("a stale maintainer /reopen cannot reverse a later close", async () => {
  const f = closedFixture({ commenter: "maintainer", permission: { permission: "admin" }, live: { closed_at: "2026-10-22T04:01:00Z" } });
  f.context.payload.comment.created_at = "2026-10-22T04:00:00Z";
  await reopen(f);
  assert.equal(f.issue.state, "closed");
  assert.equal(f.calls.filter((call) => call.method === "permission").length, 0);
});

for (const association of ["NONE", "OWNER", "MEMBER", "COLLABORATOR"]) {
  test(`the nightly check expires incomplete ${association} issues`, async () => {
    const f = graceFixture({ issue: { author_association: association } });
    await expire(f);
    assert.equal(f.issue.state, "closed");
    assert.deepEqual(f.labels().sort(), ["invalid-template", "template-grace-used"]);
    assert.match(f.comments[0].body, /maintainer can comment `\/reopen`/i);
  });
}

test("the nightly check passes a fixed maintainer issue", async () => {
  const f = graceFixture({ issue: { author_association: "OWNER", body: completeBody } });
  await expire(f);
  assert.equal(f.issue.state, "open");
  assert.deepEqual(f.labels().sort(), ["bug", "template-grace-used"]);
});

test("the nightly dry run changes no issue data", async () => {
  const f = graceFixture();
  f.context.eventName = "workflow_dispatch";
  f.context.payload.inputs.dry_run = "true";
  await expire(f);
  assert.equal(f.issue.state, "open");
  assert.equal(f.comments.length, 0);
  assert.deepEqual(f.labels(), ["invalid-template", "template-grace", "template-grace-used"]);
});

test("the same issue can complete repeated maintainer reopen and expiry cycles", async () => {
  const f = closedFixture({ commenter: "maintainer", permission: { permission: "admin" }, issue: { author_association: "OWNER", labels: ["invalid-template", "template-grace-used"] } });
  for (let cycle = 0; cycle < 3; cycle++) {
    f.context.eventName = "issue_comment";
    f.context.payload.issue.state = "closed";
    f.context.payload.comment.id++;
    await reopen(f);
    assert.equal(f.issue.state, "open");
    f.context.eventName = "schedule";
    await expire(f);
    assert.equal(f.issue.state, "closed");
    assert.ok(!f.labels().includes("template-grace"));
    assert.ok(f.labels().includes("template-grace-used"));
  }
  assert.equal(f.comments.filter((comment) => comment.body.includes("meetily:grace-reopened:")).length, 3);
  assert.equal(f.comments.filter((comment) => comment.body.includes("meetily:grace-expired")).length, 3);
});

test("a failed expiry retry does not repeat the same cycle's comment", async () => {
  const f = graceFixture({ failUpdate: true });
  await expire(f);
  await expire(f);
  assert.equal(f.comments.length, 1);
  assert.ok(f.labels().includes("template-grace"));
  assert.equal(f.calls.filter((call) => call.method === "warning").length, 2);
});

test("close cleanup removes only the active grace label", async () => {
  const f = fixture({ action: "closed", issue: { state: "closed", labels: ["invalid-template", "template-grace", "template-grace-used"] } });
  await cleanup(f);
  assert.deepEqual(f.labels(), ["invalid-template", "template-grace-used"]);
});

test("a delayed close event cannot remove a newly granted grace window", async () => {
  const f = fixture({ action: "closed", issue: { state: "closed", labels: ["invalid-template", "template-grace", "template-grace-used"] }, live: { state: "open" } });
  await cleanup(f);
  assert.ok(f.labels().includes("template-grace"));
});

test("the shared validator rejects empty sections, unchecked boxes, and headings inside code", () => {
  const { validate } = require("./issue-template.js");
  assert.equal(validate({ title: "[BUG] Example", body: completeBody }).ok, true);
  for (const body of [
    completeBody.replace("macOS 15", ""),
    completeBody.replace("macOS 15", "_No response_"),
    completeBody.replace("- [x] I have described", "- [ ] I have described"),
    completeBody.replace("### Environment\nmacOS 15", "```\n### Environment\nmacOS 15\n```"),
  ]) assert.equal(validate({ title: "[BUG] Example", body }).ok, false);
  assert.equal(validate({ title: "No prefix", body: completeBody }).ok, false);
});
