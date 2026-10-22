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
  const allowed = new Function("github", "contains", `return (${guard});`);
  const shouldRun = (fixture) => {
    const contains = (value, search) => String(value || "").toLowerCase().includes(search.toLowerCase());
    return allowed({ event_name: fixture.context.eventName, event: fixture.context.payload }, contains);
  };
  const runJob = async (fixture) => {
    if (!shouldRun(fixture)) return;
    await run(fixture.context, fixture.github, fixture.core, require);
  };
  runJob.shouldRun = shouldRun;
  return runJob;
}

const enforce = loadJob("issue-enforce.yml", "enforce");
const reopen = loadJob("issue-grace.yml", "reopen");
const sweep = loadJob("issue-grace.yml", "sweep");

const INVALID = "invalid-template";
const GRACE = "template-grace";
const AGAIN = "invalid-template-again";
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
  const issues = new Map([[issue.number, issue], ...(options.extraIssues || []).map((extra) =>
    [extra.number, { ...structuredClone(issue), ...extra }]
  )]);
  const calls = [];
  const comments = (options.comments || []).map((body, id) => ({ id, body, user: { type: "Bot" } }));
  const threads = new Map([[issue.number, comments]]);
  const context = {
    repo: { owner: "owner", repo: "repo" },
    actor: "author",
    runId: 5000,
    eventName: options.eventName || "issues",
    payload: {
      action: options.action || "opened",
      issue: structuredClone(issue),
      comment: { id: 100, body: "/reopen", user: { login: options.commenter || "author" } },
      inputs: { dry_run: "false" },
    },
  };
  Object.assign(issue, options.live);
  const labels = (number = issue.number) => issues.get(number).labels.map((label) => label.name || label);
  const failure = (message) => Object.assign(new Error(message), { status: 500 });
  const fails = (name, number) => options[name] === true || options[name] === number;
  const thread = (number) => {
    if (!threads.has(number)) threads.set(number, []);
    return threads.get(number);
  };
  const page = (items, args) => items.slice((args.page - 1) * args.per_page, args.page * args.per_page);
  const github = {
    rest: {
      issues: {
        get: async (args) => {
          calls.push({ method: "get", ...args });
          const live = issues.get(args.issue_number);
          if (options.beforeGet) options.beforeGet(live);
          return { data: structuredClone(live) };
        },
        listForRepo: async (args) => {
          calls.push({ method: "listForRepo", ...args });
          const matches = Array.from(issues.values()).filter((item) =>
            item.state === args.state && (!args.labels || labels(item.number).includes(args.labels))
          );
          return { data: structuredClone(page(matches, args)) };
        },
        listComments: async (args) => ({ data: structuredClone(page(thread(args.issue_number), args)) }),
        createComment: async ({ body, issue_number }) => {
          calls.push({ method: "createComment", body, issue_number });
          if (fails("failComment", issue_number)) throw failure("Comment failed");
          const entries = thread(issue_number);
          entries.push({ id: 200 + entries.length, body, user: { type: "Bot" } });
        },
        addLabels: async ({ labels: added, issue_number }) => {
          calls.push({ method: "addLabels", labels: added, issue_number, state: issues.get(issue_number).state });
          if (options.failLabel && added.includes(options.failLabel)) throw failure("Label failed");
          for (const label of added) if (!labels(issue_number).includes(label)) issues.get(issue_number).labels.push({ name: label });
        },
        removeLabel: async ({ name, issue_number }) => {
          calls.push({ method: "removeLabel", name, issue_number, state: issues.get(issue_number).state });
          if (options.failRemove === name) throw failure("Label removal failed");
          const live = issues.get(issue_number);
          live.labels = live.labels.filter((label) => (label.name || label) !== name);
        },
        createLabel: async (args) => { calls.push({ method: "createLabel", ...args }); },
        update: async (args) => {
          calls.push({ method: "update", labels: labels(args.issue_number), ...args });
          if (fails("failUpdate", args.issue_number)) throw failure("Update failed");
          if (args.state) issues.get(args.issue_number).state = args.state;
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
    paginate: async (method, args) => {
      const result = [];
      for (let currentPage = 1; ; currentPage++) {
        const { data } = await method({ ...args, page: currentPage });
        result.push(...data);
        if (data.length < args.per_page) return result;
      }
    },
  };
  const core = {
    notice: () => {},
    info: () => {},
    warning: (message) => { calls.push({ method: "warning", message }); },
  };
  const event = (eventName, action = "opened") => {
    context.eventName = eventName;
    context.payload.action = action;
    context.payload.issue = structuredClone(issue);
    context.runId++;
  };
  return { context, github, core, issue, issues, calls, comments, labels, event };
}

function closedFixture(options = {}) {
  return fixture({
    ...options,
    eventName: "issue_comment",
    issue: { state: "closed", labels: [INVALID], ...options.issue },
  });
}

function graceFixture(options = {}) {
  return fixture({
    ...options,
    eventName: "schedule",
    issue: { labels: [INVALID, GRACE, AGAIN], ...options.issue },
  });
}

for (const association of ["NONE", "OWNER", "MEMBER", "COLLABORATOR"]) {
  for (const action of ["opened", "edited", "reopened"]) {
    test(`${association} issue: ${action} enforces the template`, async () => {
      const f = fixture({ issue: { author_association: association }, action });
      f.context.actor = "maintainer";
      await enforce(f);
      assert.equal(f.issue.state, "closed");
      assert.ok(f.labels().includes(INVALID));
      assert.equal(f.comments.length, 1);
      assert.match(f.comments[0].body, /### Environment/);
      assert.match(f.comments[0].body, /required boxes/);
      assert.match(f.comments[0].body, /Anyone can comment `\/reopen`/);
      assert.equal(f.calls.filter((call) => call.method === "permission").length, 0);
    });
  }
}

test("a complete issue gets its matching label and an explanation", async () => {
  const f = fixture({ issue: { author_association: "OWNER", body: completeBody } });
  await enforce(f);
  assert.equal(f.issue.state, "open");
  assert.deepEqual(f.labels(), ["bug"]);
  assert.match(f.comments[0].body, /add the `bug` label/);
});

test("a complete issue with correct labels needs no changes or comment", async () => {
  const f = fixture({ issue: { body: completeBody, labels: ["bug"] } });
  await enforce(f);
  assert.equal(f.issue.state, "open");
  assert.equal(f.comments.length, 0);
  assert.deepEqual(f.calls.map((call) => call.method), ["get"]);
});

test("a missing title prefix closes with links to all four forms", async () => {
  const f = fixture({ issue: { title: "The app stops" } });
  await enforce(f);
  assert.equal(f.issue.state, "closed");
  for (const form of ["bug_report.yml", "feature_request.yml", "documentation.yml", "question.yml"]) {
    assert.ok(f.comments[0].body.includes("issues/new?template=" + form));
  }
});

test("enforcement uses the current body and state", async () => {
  const fixed = fixture({ live: { body: completeBody } });
  await enforce(fixed);
  assert.equal(fixed.issue.state, "open");
  assert.ok(fixed.labels().includes("bug"));
  const closed = fixture({ live: { state: "closed" } });
  await enforce(closed);
  assert.equal(closed.comments.length, 0);
  assert.deepEqual(closed.calls.map((call) => call.method), ["get"]);
});

for (const action of ["edited", "unlabeled", "closed"]) {
  test(`a closed issue receives no automatic work on ${action}`, async () => {
    const f = fixture({ issue: { state: "closed", labels: [INVALID, GRACE, AGAIN] }, action });
    await enforce(f);
    assert.deepEqual(f.calls, []);
    assert.deepEqual(f.labels(), [INVALID, GRACE, AGAIN]);
  });
}

test("there is no workflow subscription for closed issue cleanup", () => {
  const source = fs.readFileSync(path.join(root, ".github/workflows/issue-grace.yml"), "utf8");
  assert.doesNotMatch(source, /^  issues:/m);
  assert.doesNotMatch(source, /^  cleanup:/m);
});

test("removing grace checks and closes an incomplete issue", async () => {
  const f = fixture({ action: "unlabeled", issue: { labels: [INVALID, AGAIN] } });
  f.context.payload.label = { name: GRACE };
  await enforce(f);
  assert.equal(f.issue.state, "closed");
  assert.match(f.comments[0].body, /Ask a maintainer/);
});

test("removing an unrelated label does not start a template check", async () => {
  const f = fixture({ action: "unlabeled" });
  f.context.payload.label = { name: "question" };
  await enforce(f);
  assert.deepEqual(f.calls, []);
});

test("a manual maintainer reopen without grace has the normal checks", async () => {
  const f = fixture({ action: "reopened", issue: { labels: [INVALID, AGAIN] } });
  f.context.actor = "maintainer";
  await enforce(f);
  assert.equal(f.issue.state, "closed");
  assert.ok(!f.labels().includes(GRACE));
  assert.match(f.comments[0].body, /maintainer/);
});

test("retained grace is intentionally allowed after an author manually closes and reopens the issue", async () => {
  const f = graceFixture();
  f.issue.state = "closed";
  f.event("issues", "closed");
  await enforce(f);
  f.issue.state = "open";
  f.event("issues", "reopened");
  await enforce(f);
  assert.equal(f.issue.state, "open");
  assert.deepEqual(f.labels(), [INVALID, GRACE, AGAIN]);
  assert.equal(f.comments.length, 0);
});

test("grace allows repeated partial edits, including edits after a complete correction", async () => {
  const f = graceFixture({ issue: { author_association: "OWNER" } });
  for (const body of ["partial fix", completeBody, "another partial fix"]) {
    f.issue.body = body;
    f.event("issues", "edited");
    await enforce(f);
    assert.equal(f.issue.state, "open");
    assert.deepEqual(f.labels(), [INVALID, GRACE, AGAIN]);
  }
  assert.equal(f.comments.length, 0);
});

test("a complete edit outside grace clears invalid status and explains why", async () => {
  const f = fixture({ action: "edited", issue: { body: completeBody, labels: [INVALID, AGAIN] } });
  await enforce(f);
  assert.deepEqual(f.labels().sort(), ["bug", AGAIN].sort());
  assert.match(f.comments[0].body, /remove these labels: `invalid-template`/);
});

for (const commenter of ["author", "another-user"]) {
  test(`${commenter} can use the first reopen without a permission check`, async () => {
    const f = closedFixture({ commenter, failPermission: true });
    await reopen(f);
    assert.equal(f.issue.state, "open");
    const update = f.calls.find((call) => call.method === "update");
    assert.ok(update.labels.includes(GRACE));
    assert.ok(update.labels.includes(AGAIN));
    assert.equal(f.calls.filter((call) => call.method === "permission").length, 0);
    assert.match(f.comments[0].body, /04:00 UTC/);
    assert.match(f.comments[0].body, /as many edits as needed/);
  });
}

test("the first reopen always grants grace and records the allowance, even after a complete correction", async () => {
  const f = closedFixture({ issue: { body: completeBody } });
  await reopen(f);
  assert.equal(f.issue.state, "open");
  assert.deepEqual(f.labels().sort(), [INVALID, GRACE, AGAIN].sort());
  assert.ok(!f.labels().includes("bug"));
});

test("an existing repository label does not prevent a reopen", async () => {
  const f = closedFixture();
  f.github.rest.issues.createLabel = async () => {
    throw Object.assign(new Error("Label already exists"), { status: 422 });
  };
  await reopen(f);
  assert.equal(f.issue.state, "open");
  assert.deepEqual(f.labels().sort(), [INVALID, GRACE, AGAIN].sort());
});

test("/reopen does not inspect a closed issue's title or body", async () => {
  const f = closedFixture();
  const live = { ...f.issue };
  for (const field of ["title", "body"]) Object.defineProperty(live, field, {
    get: () => { throw new Error("Closed issue content was inspected"); },
  });
  f.github.rest.issues.get = async () => ({ data: live });
  await reopen(f);
  assert.equal(f.issue.state, "open");
});

test("a different user cannot get a second allowance for the same issue", async () => {
  const f = closedFixture({ commenter: "another-user", issue: { labels: [INVALID, AGAIN] } });
  await reopen(f);
  assert.equal(f.issue.state, "closed");
  assert.match(f.comments[0].body, /Ask a maintainer/);
  await reopen(f);
  assert.equal(f.comments.length, 1, "A rerun must not repeat the same refusal");
  f.context.payload.comment.id++;
  f.context.payload.comment.user.login = "third-user";
  await reopen(f);
  assert.equal(f.issue.state, "closed");
  assert.equal(f.comments.length, 2, "Each new request needs an explanation");
});

for (const permission of [
  { permission: "admin", role_name: "admin" },
  { permission: "write", role_name: "write" },
  { permission: "write", role_name: "maintain" },
  { permission: "read", role_name: "triage" },
  { permission: "read", role_name: "custom-triage", user: { permissions: { triage: true } } },
]) {
  for (const commenter of ["author", "maintainer"]) {
    test(`${permission.role_name} can grant more grace as ${commenter}`, async () => {
      const f = closedFixture({ commenter, permission, issue: { labels: [INVALID, AGAIN] } });
      await reopen(f);
      assert.equal(f.issue.state, "open");
      assert.ok(f.labels().includes(GRACE));
      assert.ok(f.labels().includes(AGAIN));
      assert.match(f.comments[0].body, /04:00 UTC/);
    });
  }
}

test("the permission lookup uses the commenter, not a maintainer who reruns the job", async () => {
  const f = closedFixture({ commenter: "reader", issue: { labels: [INVALID, AGAIN] } });
  f.context.actor = "maintainer";
  f.context.payload.comment.author_association = "MEMBER";
  await reopen(f);
  assert.equal(f.issue.state, "closed");
  assert.equal(f.calls.find((call) => call.method === "permission").username, "reader");
});

test("a failed permission check keeps the issue closed with an explanation", async () => {
  const f = closedFixture({ failPermission: true, issue: { labels: [INVALID, AGAIN] } });
  await reopen(f);
  assert.equal(f.issue.state, "closed");
  assert.deepEqual(f.labels(), [INVALID, AGAIN]);
  assert.match(f.comments[0].body, /could not check your maintainer permissions/);
});

test("a failed first reopen restores labels and explains the failure", async () => {
  const f = closedFixture({ failUpdate: true });
  await assert.rejects(reopen(f), /Update failed/);
  assert.deepEqual(f.labels(), [INVALID]);
  assert.match(f.comments.at(-1).body, /reopen request failed/);
});

test("a failed maintainer reopen preserves the maintainer-only label", async () => {
  const f = closedFixture({ failUpdate: true, commenter: "maintainer", permission: { permission: "write" }, issue: { labels: [INVALID, AGAIN] } });
  await assert.rejects(reopen(f), /Update failed/);
  assert.deepEqual(f.labels().sort(), [INVALID, AGAIN].sort());
});

test("a lost response after a successful reopen preserves grace and the allowance label", async () => {
  const f = closedFixture();
  const update = f.github.rest.issues.update;
  f.github.rest.issues.update = async (args) => {
    await update(args);
    throw new Error("Response lost after update");
  };
  await reopen(f);
  assert.equal(f.issue.state, "open");
  assert.deepEqual(f.labels().sort(), [INVALID, GRACE, AGAIN].sort());
  assert.match(f.comments.at(-1).body, /confirmed that this issue is open/);
  assert.ok(!f.calls.some((call) => call.method === "removeLabel"));
});

test("an unknown reopen state preserves request labels and explains the uncertainty", async () => {
  const f = closedFixture();
  const get = f.github.rest.issues.get;
  const update = f.github.rest.issues.update;
  let firstRead = true;
  f.github.rest.issues.get = async (args) => {
    if (!firstRead) throw new Error("State unavailable");
    firstRead = false;
    return get(args);
  };
  f.github.rest.issues.update = async (args) => {
    await update(args);
    throw new Error("Response lost after update");
  };
  await assert.rejects(reopen(f), /Response lost after update/);
  assert.equal(f.issue.state, "open");
  assert.deepEqual(f.labels().sort(), [INVALID, GRACE, AGAIN].sort());
  assert.match(f.comments.at(-1).body, /could not confirm the issue's current state/);
  assert.ok(!f.calls.some((call) => call.method === "removeLabel"));
});

test("a lost label response rolls back the actual labels on a still-closed issue", async () => {
  const f = closedFixture();
  const add = f.github.rest.issues.addLabels;
  f.github.rest.issues.addLabels = async (args) => {
    await add(args);
    throw new Error("Response lost after label update");
  };
  await assert.rejects(reopen(f), /Response lost after label update/);
  assert.equal(f.issue.state, "closed");
  assert.deepEqual(f.labels(), [INVALID]);
});

test("a later manual closure after a lost reopen response keeps the request labels", async () => {
  const f = closedFixture({ issue: { closed_at: "2026-10-22T01:00:00Z" } });
  const update = f.github.rest.issues.update;
  f.github.rest.issues.update = async (args) => {
    await update(args);
    f.issue.state = "closed";
    f.issue.closed_at = "2026-10-22T02:00:00Z";
    throw new Error("Response lost after update");
  };
  await reopen(f);
  assert.equal(f.issue.state, "closed");
  assert.deepEqual(f.labels().sort(), [INVALID, GRACE, AGAIN].sort());
  assert.match(f.comments.at(-1).body, /later closure/);
});

test("a failed label addition does not use the first allowance", async () => {
  const f = closedFixture({ failLabel: AGAIN });
  await assert.rejects(reopen(f), /Label failed/);
  assert.equal(f.issue.state, "closed");
  assert.deepEqual(f.labels(), [INVALID]);
});

test("a rollback failure is explained and does not stop the other label rollback", async () => {
  const f = closedFixture({ failUpdate: true, failRemove: GRACE });
  await assert.rejects(reopen(f), /Update failed/);
  assert.deepEqual(f.labels(), [INVALID, GRACE]);
  assert.match(f.comments.at(-1).body, /could not remove these request labels: `template-grace`/);
  assert.match(f.comments.at(-1).body, /Ask a maintainer/);
});

test("open issues, pull requests, and ordinary comments cause no reopen action", async () => {
  const open = closedFixture({ issue: { state: "open" } });
  const pr = closedFixture({ issue: { pull_request: {} } });
  const ordinary = closedFixture();
  ordinary.context.payload.comment.body = "Please reopen this";
  for (const f of [open, pr, ordinary]) {
    await reopen(f);
    assert.deepEqual(f.calls, []);
  }
});

test("the job guard filters ordinary comments before loading the command script", async () => {
  const f = closedFixture();
  f.context.payload.comment.body = "Thanks for the help";
  assert.equal(reopen.shouldRun(f), false);
  await reopen(f);
  assert.deepEqual(f.calls, []);
});

test("command filtering preserves case and surrounding whitespace", async () => {
  const f = closedFixture();
  f.context.payload.comment.body = "  /REOPEN\n";
  assert.equal(reopen.shouldRun(f), true);
  await reopen(f);
  assert.equal(f.issue.state, "open");
});

test("both issue jobs preserve pending work in the same queue", () => {
  for (const [file, job] of [["issue-enforce.yml", "enforce"], ["issue-grace.yml", "reopen"]]) {
    const source = fs.readFileSync(path.join(root, ".github/workflows", file), "utf8");
    const block = source.match(new RegExp(`^  ${job}:\\n([\\s\\S]*?)(?=^  [\\w-]+:|$(?![\\s\\S]))`, "m"))[1];
    assert.match(block, /      group: issue-template-\$\{\{ github.event.issue.number \}\}/);
    assert.match(block, /      queue: max\b/);
    assert.match(block, /      cancel-in-progress: false\b/);
    assert.doesNotMatch(source, /^concurrency:/m, "Skipped closed-issue events must not enter the shared queue");
  }
});

test("a non-template closure gets a refusal explanation", async () => {
  const f = closedFixture({ issue: { labels: ["duplicate"] } });
  await reopen(f);
  assert.equal(f.issue.state, "closed");
  assert.match(f.comments[0].body, /applies only to issues closed for missing template information/);
});

test("a stale /reopen cannot reverse a later close", async () => {
  const f = closedFixture({ live: { closed_at: "2026-10-22T04:01:00Z" } });
  f.context.payload.comment.created_at = "2026-10-22T04:00:00Z";
  await reopen(f);
  assert.equal(f.issue.state, "closed");
  assert.match(f.comments[0].body, /before its latest closure/);
});

for (const association of ["NONE", "OWNER", "MEMBER", "COLLABORATOR"]) {
  test(`the nightly check closes incomplete ${association} issues and lists the current failures`, async () => {
    const f = graceFixture({ issue: { author_association: association } });
    await sweep(f);
    assert.equal(f.issue.state, "closed");
    assert.deepEqual(f.labels().sort(), [INVALID, AGAIN].sort());
    assert.match(f.comments[0].body, /### Environment/);
    assert.match(f.comments[0].body, /required boxes/);
    assert.match(f.comments[0].body, /maintainer/);
    assert.ok(f.calls.filter((call) => call.method === "removeLabel").every((call) => call.state === "open"));
  });
}

test("the nightly check includes older issues without any labels", async () => {
  const f = fixture({ eventName: "schedule" });
  await sweep(f);
  assert.equal(f.issue.state, "closed");
  assert.ok(f.labels().includes(INVALID));
  const request = f.calls.find((call) => call.method === "listForRepo");
  assert.equal(request.state, "open");
  assert.equal(request.labels, undefined);
});

test("the nightly check clears grace and invalid status when the issue is complete", async () => {
  const f = graceFixture({ issue: { author_association: "OWNER", body: completeBody } });
  await sweep(f);
  assert.equal(f.issue.state, "open");
  assert.deepEqual(f.labels().sort(), ["bug", AGAIN].sort());
  assert.match(f.comments[0].body, /remove these labels: `invalid-template`, `template-grace`/);
});

test("the nightly check leaves a complete issue with correct labels alone", async () => {
  const f = fixture({ eventName: "schedule", issue: { body: completeBody, labels: ["bug"] } });
  await sweep(f);
  assert.equal(f.comments.length, 0);
  assert.deepEqual(f.calls.map((call) => call.method), ["listForRepo", "get"]);
});

test("closed issues and pull requests are not read by the nightly check", async () => {
  const f = fixture({ eventName: "schedule", issue: { state: "closed", labels: [INVALID, GRACE, AGAIN] }, extraIssues: [
    { number: 2, state: "open", pull_request: {} },
  ] });
  await sweep(f);
  assert.equal(f.calls.filter((call) => call.method === "get").length, 0);
  assert.deepEqual(f.labels(), [INVALID, GRACE, AGAIN]);
});

test("the nightly check skips an issue closed after listing", async () => {
  const f = graceFixture({ beforeGet: (issue) => { issue.state = "closed"; } });
  await sweep(f);
  assert.deepEqual(f.calls.map((call) => call.method), ["listForRepo", "get"]);
  assert.deepEqual(f.labels(), [INVALID, GRACE, AGAIN]);
});

test("the nightly check does not inspect content after an issue closes", async () => {
  const f = graceFixture();
  const closed = { ...f.issue, state: "closed" };
  for (const field of ["title", "body"]) Object.defineProperty(closed, field, {
    get: () => { throw new Error("Closed issue content was inspected"); },
  });
  f.github.rest.issues.get = async () => ({ data: closed });
  await sweep(f);
  assert.equal(f.comments.length, 0);
  assert.deepEqual(f.labels(), [INVALID, GRACE, AGAIN]);
});

test("the nightly check uses a correction made after listing", async () => {
  const f = graceFixture({ beforeGet: (issue) => { issue.body = completeBody; } });
  await sweep(f);
  assert.equal(f.issue.state, "open");
  assert.deepEqual(f.labels().sort(), ["bug", AGAIN].sort());
});

for (const [name, run] of [["event check", enforce], ["nightly check", sweep]]) {
  test(`${name} stops if a correction arrives during the comment lookup`, async () => {
    const f = run === sweep ? graceFixture() : fixture();
    const list = f.github.rest.issues.listComments;
    f.github.rest.issues.listComments = async (args) => {
      f.issue.body = completeBody;
      return list(args);
    };
    await run(f);
    assert.equal(f.issue.state, "open");
    assert.ok(!f.calls.some((call) => ["createComment", "addLabels", "removeLabel", "update"].includes(call.method)));
  });

  test(`${name} stops if a manual closure arrives during the comment lookup`, async () => {
    const f = run === sweep ? graceFixture() : fixture();
    const list = f.github.rest.issues.listComments;
    f.github.rest.issues.listComments = async (args) => {
      f.issue.state = "closed";
      return list(args);
    };
    await run(f);
    assert.equal(f.issue.state, "closed");
    assert.ok(!f.calls.some((call) => ["createComment", "addLabels", "removeLabel", "update"].includes(call.method)));
  });
}

for (const method of ["createComment", "createLabel", "addLabels", "removeLabel"]) {
  test(`a correction after ${method} stops the remaining closure actions`, async () => {
    const f = method === "removeLabel" ? graceFixture() : fixture();
    const original = f.github.rest.issues[method];
    f.github.rest.issues[method] = async (args) => {
      const result = await original(args);
      f.issue.body = completeBody;
      return result;
    };
    await (method === "removeLabel" ? sweep : enforce)(f);
    assert.equal(f.issue.state, "open");
    assert.ok(!f.calls.some((call) => call.method === "update"));
  });

  test(`a manual closure after ${method} stops the remaining issue writes`, async () => {
    const f = method === "removeLabel" ? graceFixture() : fixture();
    const original = f.github.rest.issues[method];
    let afterClosure;
    f.github.rest.issues[method] = async (args) => {
      const result = await original(args);
      f.issue.state = "closed";
      afterClosure = f.calls.length;
      return result;
    };
    await (method === "removeLabel" ? sweep : enforce)(f);
    assert.ok(f.calls.slice(afterClosure).every((call) => call.method === "get"));
  });
}

test("a new grace label during a closure comment stops the old decision", async () => {
  const f = fixture();
  const comment = f.github.rest.issues.createComment;
  f.github.rest.issues.createComment = async (args) => {
    await comment(args);
    f.issue.labels.push({ name: GRACE });
  };
  await enforce(f);
  assert.equal(f.issue.state, "open");
  assert.deepEqual(f.labels(), [GRACE]);
});

test("an incomplete edit during a pass comment stops label cleanup", async () => {
  const f = graceFixture({ issue: { body: completeBody } });
  const comment = f.github.rest.issues.createComment;
  f.github.rest.issues.createComment = async (args) => {
    await comment(args);
    f.issue.body = "An incomplete edit";
  };
  await sweep(f);
  assert.equal(f.issue.state, "open");
  assert.deepEqual(f.labels(), [INVALID, GRACE, AGAIN]);
});

test("the nightly check covers every page of open issues", async () => {
  const f = fixture({ eventName: "schedule", extraIssues: Array.from({ length: 101 }, (_, i) => ({ number: i + 2 })) });
  await sweep(f);
  assert.equal(new Set(f.calls.filter((call) => call.method === "get").map((call) => call.issue_number)).size, 102);
  assert.equal(f.calls.filter((call) => call.method === "update").length, 102);
  assert.ok(Array.from(f.issues.values()).every((issue) => issue.state === "closed"));
});

test("a nightly dry run changes neither complete nor incomplete issues", async () => {
  const f = graceFixture({ extraIssues: [{ number: 2, body: completeBody }] });
  f.context.eventName = "workflow_dispatch";
  f.context.payload.inputs.dry_run = "true";
  await sweep(f);
  assert.ok(f.calls.every((call) => ["listForRepo", "get"].includes(call.method)));
  assert.deepEqual(f.labels(), [INVALID, GRACE, AGAIN]);
  assert.equal(f.issue.state, "open");
  assert.equal(f.issues.get(2).state, "open");
});

test("a failure on one issue does not stop checks on the other issues", async () => {
  const f = graceFixture({ failUpdate: 1, extraIssues: [{ number: 2 }] });
  await assert.rejects(sweep(f), /failed for issues: 1/);
  assert.equal(f.issue.state, "open");
  assert.equal(f.issues.get(2).state, "closed");
});

for (const error of [
  { status: 429, message: "Too many requests" },
  { status: 403, message: "API rate limit exceeded" },
  { status: 403, message: "Forbidden", response: { headers: { "x-ratelimit-remaining": "0" } } },
  { status: 403, message: "Forbidden", response: { headers: { "retry-after": "60" } } },
]) {
  test(`the nightly check stops after a rate limit: ${JSON.stringify(error)}`, async () => {
    const f = graceFixture({ extraIssues: [{ number: 2 }] });
    f.github.rest.issues.update = async () => { throw Object.assign(new Error(error.message), error); };
    await assert.rejects(sweep(f), /stopped at #1.*rate limit/);
    assert.ok(f.calls.every((call) => call.issue_number !== 2), "Do not call the API for later issues after a rate limit");
  });
}

test("an ordinary permission error does not stop checks on the other issues", async () => {
  const f = graceFixture({ extraIssues: [{ number: 2 }] });
  const update = f.github.rest.issues.update;
  f.github.rest.issues.update = async (args) => {
    if (args.issue_number === 1) throw Object.assign(new Error("Resource not accessible by integration"), { status: 403 });
    return update(args);
  };
  await assert.rejects(sweep(f), /failed for issues: 1/);
  assert.equal(f.issues.get(2).state, "closed");
});

test("the same issue can complete repeated maintainer reopen and nightly check cycles", async () => {
  const f = closedFixture({ commenter: "maintainer", permission: { permission: "admin" }, issue: { author_association: "OWNER", labels: [INVALID, AGAIN] } });
  for (let cycle = 0; cycle < 3; cycle++) {
    f.event("issue_comment");
    f.context.payload.comment.id++;
    await reopen(f);
    assert.equal(f.issue.state, "open");
    f.event("schedule");
    await sweep(f);
    assert.equal(f.issue.state, "closed");
    assert.ok(!f.labels().includes(GRACE));
    assert.ok(f.labels().includes(AGAIN));
  }
  assert.equal(f.comments.filter((comment) => comment.body.includes("meetily:reopen:")).length, 3);
  assert.equal(f.comments.filter((comment) => comment.body.includes("meetily:template-check:")).length, 3);
});

test("each closure explains the current failures even if the issue already has invalid-template", async () => {
  const f = fixture({ issue: { labels: [INVALID, AGAIN] } });
  await enforce(f);
  assert.equal(f.comments.length, 1);
  f.issue.state = "open";
  f.issue.body = completeBody.replace("macOS 15", "");
  f.event("issues", "edited");
  await enforce(f);
  assert.equal(f.comments.length, 2);
  assert.match(f.comments[1].body, /### Environment/);
  assert.doesNotMatch(f.comments[1].body, /### Expected Behavior/);
});

test("a retry of the same failed closure does not repeat its comment", async () => {
  const f = fixture({ failUpdate: true });
  await assert.rejects(enforce(f), /Update failed/);
  await assert.rejects(enforce(f), /Update failed/);
  assert.equal(f.comments.length, 1);
});

test("later nightly runs reuse the explanation after a partial closure fails", async () => {
  const f = graceFixture({ failUpdate: true });
  for (let attempt = 0; attempt < 3; attempt++) {
    f.event("schedule");
    await assert.rejects(sweep(f), /failed for issues: 1/);
    assert.equal(f.issue.state, "open");
  }
  assert.equal(f.comments.length, 1);
  assert.ok(!f.labels().includes(GRACE));
  assert.match(f.comments[0].body, /remove `template-grace`/);
});

test("rerunning a failed check explains a changed invalid body", async () => {
  const f = fixture({ failUpdate: true });
  await assert.rejects(enforce(f), /Update failed/);
  f.issue.body = completeBody.replace("macOS 15", "");
  await assert.rejects(enforce(f), /Update failed/);
  assert.equal(f.comments.length, 2);
  assert.match(f.comments[1].body, /### Environment/);
  assert.doesNotMatch(f.comments[1].body, /### Expected Behavior/);
});

test("a manual reopen gets a fresh closure explanation even for an unchanged body", async () => {
  const options = { failUpdate: true };
  const f = fixture(options);
  await assert.rejects(enforce(f), /Update failed/);
  options.failUpdate = false;
  f.event("schedule");
  await sweep(f);
  assert.equal(f.issue.state, "closed");
  assert.equal(f.comments.length, 1);
  f.issue.state = "open";
  options.failUpdate = true;
  f.event("issues", "reopened");
  await assert.rejects(enforce(f), /Update failed/);
  await assert.rejects(enforce(f), /Update failed/);
  assert.equal(f.comments.length, 2);
  f.event("schedule");
  await assert.rejects(sweep(f), /failed for issues: 1/);
  assert.equal(f.comments.length, 2);
});

test("a successful check ends comment reuse before the same invalid body returns", async () => {
  const f = fixture();
  const invalidBody = f.issue.body;
  await enforce(f);
  f.issue.state = "open";
  f.issue.body = completeBody;
  f.event("issues", "edited");
  await enforce(f);
  f.issue.body = invalidBody;
  f.event("issues", "edited");
  await enforce(f);
  assert.equal(f.comments.length, 3);
});

test("a changed repeat allowance gets an updated closure explanation", async () => {
  const f = fixture({ failUpdate: true });
  await assert.rejects(enforce(f), /Update failed/);
  f.issue.labels.push({ name: AGAIN });
  f.event("schedule");
  await assert.rejects(sweep(f), /failed for issues: 1/);
  assert.equal(f.comments.length, 2);
  assert.match(f.comments[1].body, /Ask a maintainer/);
});

for (const operation of ["close", "reopen"]) {
  test(`${operation} posts an explanation before any label or state change`, async () => {
    const f = operation === "close" ? fixture() : closedFixture();
    await (operation === "close" ? enforce : reopen)(f);
    const explanation = f.calls.findIndex((call) => call.method === "createComment");
    const action = f.calls.findIndex((call) => ["addLabels", "removeLabel", "update"].includes(call.method));
    assert.ok(explanation >= 0 && explanation < action);
  });
  test(`${operation} takes no action if the explanation cannot be posted`, async () => {
    const f = operation === "close" ? fixture({ failComment: true }) : closedFixture({ failComment: true });
    await assert.rejects((operation === "close" ? enforce : reopen)(f), /Comment failed/);
    assert.ok(f.calls.every((call) => !["addLabels", "removeLabel", "update"].includes(call.method)));
  });
}

test("a user cannot suppress the bot's explanation with a copied comment marker", async () => {
  const f = fixture({ failUpdate: true });
  await assert.rejects(enforce(f), /Update failed/);
  // Leave only a user-authored copy of the exact markers this check uses.
  f.comments[0].user.type = "User";
  await assert.rejects(enforce(f), /Update failed/);
  assert.equal(f.comments.length, 2);
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

test("nested Markdown headings remain part of their required field", () => {
  const { validate } = require("./issue-template.js");
  for (const level of [4, 5, 6]) {
    const body = completeBody.replace("### Bug Description\nThe app stops.", "### Bug Description\n" + "#".repeat(level) + " Details\nThe app stops.");
    assert.equal(validate({ title: "[BUG] Example", body }).ok, true);
  }
});

test("existing level-one and level-two section headings still pass", () => {
  const { validate } = require("./issue-template.js");
  for (const level of [1, 2]) {
    const body = completeBody.replace(/^### /gm, "#".repeat(level) + " ");
    assert.equal(validate({ title: "[BUG] Example", body }).ok, true);
  }
});

test("code samples and indented code cannot satisfy required task-list boxes", () => {
  const { validate } = require("./issue-template.js");
  const [fields, boxes] = completeBody.split("### Checklist\n");
  for (const code of [
    "```text\n" + boxes + "\n```",
    "~~~text\n" + boxes + "\n~~~",
    "````text\n```\n" + boxes + "\n```\n````",
    boxes.split("\n").map((line) => "    " + line).join("\n"),
    boxes.split("\n").map((line) => "\t" + line).join("\n"),
  ]) {
    const result = validate({ title: "[BUG] Example", body: fields + "### Checklist\n" + code });
    assert.equal(result.ok, false);
    assert.equal(result.unchecked.length, 2);
  }
});

test("short or annotated fences do not end an outer code block", () => {
  const { validate } = require("./issue-template.js");
  for (const inner of ["```", "````text", "~~~"]) {
    const body = completeBody.replace("### Environment\nmacOS 15", "````text\n" + inner + "\n### Environment\nmacOS 15\n````");
    assert.equal(validate({ title: "[BUG] Example", body }).ok, false);
  }
});

test("code fences on list-item lines do not hide later issue fields", async () => {
  for (const [opening, indent] of [["- ```text", "  "], ["-\t```text", "\t"], ["1. ```text", "   "],
    ["123. ~~~text", "     "], ["  - ````text", "    "]]) {
    const fence = opening.includes("~") ? "~~~" : opening.includes("````") ? "````" : "```";
    const body = completeBody.replace("The app stops.",
      "The app stops.\n\n" + opening + "\n" + indent + "Example output\n" + indent + fence);
    const f = fixture({ issue: { body } });
    await enforce(f);
    assert.equal(f.issue.state, "open", opening);
    assert.deepEqual(f.labels(), ["bug"]);
  }
});

test("leaving a list item ends its unclosed code fence", () => {
  const { validate } = require("./issue-template.js");
  const body = completeBody.replace("The app stops.", "- ```text\n  Example output");
  assert.equal(validate({ title: "[BUG] Test", body }).ok, true);
});

test("headings and checklist boxes inside list-item fences remain code", () => {
  const { validate } = require("./issue-template.js");
  const body = "### Bug Description\n- ```text\n" +
    completeBody.split("\n").map((line) => "  " + line).join("\n") + "\n  ```";
  const result = validate({ title: "[BUG] Test", body });
  assert.equal(result.ok, false);
  assert.deepEqual(result.missing, ["Current Behavior", "Expected Behavior", "Steps to Reproduce", "Environment"]);
  assert.equal(result.unchecked.length, 2);
});

test("fences on list-continuation lines end with their list", async () => {
  for (const example of [
    "- Steps:\n  ```text\n  run app",
    "1. Steps:\n   ```text\n   run app",
    "- Outer:\n  - Steps:\n    ```text\n    run app",
    "- Steps:\n     ```text\n     run app",
    "- Steps:\n\t```text\n\trun app",
  ]) {
    const body = completeBody.replace("The app stops.", "The app stops.\n\n" + example);
    for (const check of [enforce, sweep]) {
      const f = fixture({ eventName: check === sweep ? "schedule" : "issues", issue: { body } });
      await check(f);
      assert.equal(f.issue.state, "open", example);
      assert.deepEqual(f.labels(), ["bug"]);
    }
  }
});

test("an outdented fence after a list opens a separate top-level code block", () => {
  const { validate } = require("./issue-template.js");
  const body = completeBody.replace("The app stops.", "The app stops.\n\n- Steps:\n  ```text\n  run app\n```");
  const result = validate({ title: "[BUG] Test", body });
  assert.equal(result.ok, false);
  assert.deepEqual(result.missing, ["Current Behavior", "Expected Behavior", "Steps to Reproduce", "Environment"]);
  assert.equal(result.unchecked.length, 2);
});

const htmlBlocks = [
  ["<pre>", "</pre>"],
  ['  <PRE class="example">', "  </pRe>"],
  ["<script>", "</script>"],
  ["<style>", "</style>"],
  ["<textarea>\n", "</textarea>"],
  ['  <TEXTAREA rows="2">\n', "  </TeXtArEa>"],
  ["<!--", "-->"],
  ["<?example", "?>"],
  ["<!DOCTYPE example [", ">"],
  ["<![CDATA[", "]]>"],
  ["<div>", "</div>"],
  ["<custom-element>", "</custom-element>"],
];

test("raw HTML cannot supply required headings or task-list boxes", () => {
  const { validate } = require("./issue-template.js");
  for (const [start, end] of htmlBlocks) {
    const result = validate({ title: "[BUG] Example", body: start + "\n" + completeBody + "\n" + end });
    assert.equal(result.ok, false, start);
    assert.equal(result.missing.length, 5, start);
    assert.equal(result.unchecked.length, 2, start);
  }
});

test("an issue containing only an HTML template example closes with an explanation", async () => {
  const f = fixture({ issue: { body: "<pre>\n" + completeBody + "\n</pre>" } });
  await enforce(f);
  assert.equal(f.issue.state, "closed");
  assert.match(f.comments[0].body, /### Bug Description/);
  assert.match(f.comments[0].body, /required boxes/);
});

test("a textarea template example with blank lines closes with an explanation", async () => {
  const f = fixture({ issue: { body: "<textarea>\n\n" + completeBody + "\n</textarea>" } });
  await enforce(f);
  assert.equal(f.issue.state, "closed");
  assert.match(f.comments[0].body, /### Bug Description/);
  assert.match(f.comments[0].body, /required boxes/);
});

test("an unclosed textarea keeps later headings and boxes as raw text", () => {
  const { validate } = require("./issue-template.js");
  const body = "An example follows.\n<textarea>\n\n" + completeBody;
  const result = validate({ title: "[BUG] Example", body });
  assert.equal(result.ok, false);
  assert.equal(result.missing.length, 5);
  assert.equal(result.unchecked.length, 2);
});

test("HTML samples cannot replace unchecked checklist boxes", () => {
  const { validate } = require("./issue-template.js");
  const [fields, boxes] = completeBody.split("### Checklist\n");
  for (const [start, end] of htmlBlocks) {
    const body = fields + "### Checklist\n" + start + "\n" + boxes + "\n" + end + "\n\n" + boxes.replaceAll("[x]", "[ ]");
    const result = validate({ title: "[BUG] Example", body });
    assert.equal(result.ok, false, start);
    assert.equal(result.unchecked.length, 2, start);
  }
});

test("a hidden comment does not complete a required field", () => {
  const { validate } = require("./issue-template.js");
  const result = validate({ title: "[BUG] Example", body: completeBody.replace("macOS 15", "<!-- TODO: add the OS -->") });
  assert.deepEqual(result.missing, ["Environment"]);
});

test("Markdown resumes after an HTML block, including code fence text inside the block", () => {
  const { validate } = require("./issue-template.js");
  for (const [start, end] of htmlBlocks) {
    const body = start + "\n```\n### Example\n" + end + "\n\n" + completeBody;
    assert.equal(validate({ title: "[BUG] Example", body }).ok, true, start);
  }
});

test("blank lines allow Markdown inside HTML containers", () => {
  const { validate } = require("./issue-template.js");
  for (const start of ["<details>\n<summary>Issue details</summary>", "<div>", "<custom-element>"]) {
    const body = start + "\n\n" + completeBody + "\n\n</details>";
    assert.equal(validate({ title: "[BUG] Example", body }).ok, true, start);
  }
});

test("visible HTML field values and fenced HTML examples remain valid", () => {
  const { validate } = require("./issue-template.js");
  for (const value of ["<pre>macOS 15</pre>\n", "<div>macOS 15</div>\n"]) {
    assert.equal(validate({ title: "[BUG] Example", body: completeBody.replace("macOS 15", value) }).ok, true);
  }
  const example = "```html\n<pre>\n<!--\n### Example\n```\nThe app stops.";
  assert.equal(validate({ title: "[BUG] Example", body: completeBody.replace("The app stops.", example) }).ok, true);
});

test("an inline comment cannot hide a later Markdown heading", () => {
  const { validate } = require("./issue-template.js");
  const body = completeBody.replace("### Environment\nmacOS 15", "A note <!--\n### Environment\nmacOS 15\n-->");
  assert.equal(validate({ title: "[BUG] Example", body }).ok, true);
});

test("an inline HTML tag does not start a block in an existing paragraph", () => {
  const { validate } = require("./issue-template.js");
  const body = completeBody.replace("### Environment", "A note\n<span>\n### Environment");
  assert.equal(validate({ title: "[BUG] Example", body }).ok, true);
});

test("literal comment markers in inline code and escaped text do not hide later fields", () => {
  const { validate } = require("./issue-template.js");
  for (const text of ["The log contains `<!--`.", "The log contains `` `<!--` ``.", "The log contains \\<!--."]) {
    const body = completeBody.replace("The app stops.", text);
    assert.equal(validate({ title: "[BUG] Example", body }).ok, true, text);
  }
});

test("literal comments in indented code do not hide later fields or close a complete issue", async () => {
  const { validate } = require("./issue-template.js");
  for (const indent of ["    ", "\t", "  \t"]) {
    const body = completeBody.replace("The app stops.", "The app stops.\n\n" + indent + "<!-- literal code comment\n");
    assert.equal(validate({ title: "[BUG] Example", body }).ok, true, JSON.stringify(indent));
    const f = fixture({ issue: { body } });
    await enforce(f);
    assert.equal(f.issue.state, "open");
    assert.ok(!f.calls.some((call) => call.method === "update"));
  }
});

test("an unfinished inline comment does not hide later fields", () => {
  const { validate } = require("./issue-template.js");
  for (const text of ["The app stops. <!-- literal", "The app stops.\n    <!-- literal"]) {
    const body = completeBody.replace("The app stops.", text);
    assert.equal(validate({ title: "[BUG] Example", body }).ok, true);
  }
});

test("closed multiline inline comments remain hidden within their paragraph", () => {
  const { parseSections, validate } = require("./issue-template.js");
  const body = completeBody.replace("The app stops.", "The app stops. <!-- hidden\n    comment -->");
  assert.equal(validate({ title: "[BUG] Example", body }).ok, true);
  assert.doesNotMatch(parseSections(body).get("Bug Description").join("\n"), /hidden|comment/);
});

test("HTML blocks after thematic breaks or setext headings cannot supply issue fields", () => {
  const { validate } = require("./issue-template.js");
  for (const boundary of ["---", "***", "___", "  * * *", "\t***", "===", "-"]) {
    // The tab-indented break remains code, and its following blank line ends it.
    const prefix = boundary === "\t***" ? "A note.\n\n" + boundary + "\n\n" : "A note.\n" + boundary + "\n";
    const body = prefix + "<span>\n" + completeBody;
    const result = validate({ title: "[BUG] Example", body });
    assert.equal(result.ok, false, boundary);
    assert.equal(result.missing.length, 5, boundary);
    assert.equal(result.unchecked.length, 2, boundary);
  }
});

test("a blank line after an HTML block restores Markdown after a thematic break", () => {
  const { validate } = require("./issue-template.js");
  const body = "---\n<span>\n\n" + completeBody;
  assert.equal(validate({ title: "[BUG] Example", body }).ok, true);
});

test("HTML blocks after lists or quotes cannot supply issue fields", () => {
  const { validate } = require("./issue-template.js");
  for (const prefix of ["- item", "> item", "- item\ncontinued", "> item\ncontinued"]) {
    const result = validate({ title: "[BUG] Test", body: prefix + "\n<span>\n" + completeBody });
    assert.equal(result.ok, false, prefix);
    assert.equal(result.missing.length, 5, prefix);
    assert.equal(result.unchecked.length, 2, prefix);
  }
});

test("HTML examples inside a list do not hide fields after the list ends", () => {
  const { validate } = require("./issue-template.js");
  for (const example of ["- <pre>\n  sample", "- <pre>\n  sample\n  </pre>"]) {
    const body = completeBody.replace("The app stops.", "The app stops.\n\n" + example);
    assert.equal(validate({ title: "[BUG] Test", body }).ok, true);
  }
});

test("failed inline-comment scans do not hide closed comments in later paragraphs", () => {
  const { parseSections, validate } = require("./issue-template.js");
  const body = completeBody.replace("The app stops.", "a <!--\n".repeat(100) + "\nVisible <!-- hidden\ncomment --> text.");
  assert.equal(validate({ title: "[BUG] Test", body }).ok, true);
  assert.doesNotMatch(parseSections(body).get("Bug Description").join("\n"), /hidden|comment/);
});

test("a large paragraph of unfinished inline comments does not stall validation", () => {
  const { execFileSync } = require("node:child_process");
  const script = `const { validate } = require(${JSON.stringify(require.resolve("./issue-template.js"))});
    require('node:assert/strict').equal(validate({title:'[BUG] Test',body:'a <!--\\n'.repeat(9000)}).ok, false);`;
  execFileSync(process.execPath, ["-e", script], { timeout: 5000 });
});
