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
const INVALID = "invalid-template";
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
    number: 1, title: "[BUG] The app stops", body: "### Bug Description\nThe app stops.",
    state: "open", labels: [], ...options.issue,
  };
  const calls = [];
  const comments = (options.comments || []).map((body, id) => ({ id, body, user: { type: "Bot" } }));
  const context = {
    repo: { owner: "owner", repo: "repo" }, actor: "author", runId: 5000,
    eventName: options.eventName || "issues",
    payload: { action: options.action || "opened", issue: structuredClone(issue) },
  };
  Object.assign(issue, options.live);
  const labels = () => issue.labels.map(label => label.name || label);
  const failure = message => Object.assign(new Error(message), { status: 500 });
  const github = {
    rest: { issues: {
      get: async args => {
        calls.push({ method: "get", ...args });
        return { data: structuredClone(issue) };
      },
      listComments: async args => ({ data: structuredClone(comments.slice((args.page - 1) * args.per_page, args.page * args.per_page)) }),
      createComment: async args => {
        calls.push({ method: "createComment", ...args });
        if (options.failComment) throw failure("Comment failed");
        comments.push({ id: 200 + comments.length, body: args.body, user: { type: "Bot" } });
      },
      createLabel: async args => { calls.push({ method: "createLabel", ...args }); },
      addLabels: async args => {
        calls.push({ method: "addLabels", ...args });
        for (const label of args.labels) if (!labels().includes(label)) issue.labels.push({ name: label });
      },
      removeLabel: async args => {
        calls.push({ method: "removeLabel", ...args });
        issue.labels = issue.labels.filter(label => (label.name || label) !== args.name);
      },
      update: async args => {
        calls.push({ method: "update", ...args });
        if (options.failUpdate) throw failure("Update failed");
        if (args.state) issue.state = args.state;
      },
    } },
    paginate: async (method, args) => {
      const result = [];
      for (let page = 1; ; page++) {
        const { data } = await method({ ...args, page });
        result.push(...data);
        if (data.length < args.per_page) return result;
      }
    },
  };
  const core = { notice() {}, info() {} };
  const event = (action = "edited") => {
    context.payload.action = action;
    context.payload.issue = structuredClone(issue);
    context.runId++;
  };
  return { context, github, core, issue, calls, comments, labels, event };
}

for (const association of ["NONE", "OWNER", "MEMBER", "COLLABORATOR"]) {
  for (const action of ["opened", "edited", "reopened"]) {
    test(`${association} issue: ${action} enforces the template`, async () => {
      const f = fixture({ issue: { author_association: association }, action });
      f.context.actor = "maintainer";
      await enforce(f);
      assert.equal(f.issue.state, "closed");
      assert.deepEqual(f.labels(), [INVALID]);
      assert.equal(f.comments.length, 1);
      assert.match(f.comments[0].body, /### Environment/);
      assert.match(f.comments[0].body, /required boxes/);
      assert.match(f.comments[0].body, /ask a maintainer to reopen it/);
      assert.doesNotMatch(f.comments[0].body, /\/reopen|nightly|grace period/);
    });
  }
}

for (const valid of [false, true]) {
  test(`old recovery labels are removed on an open issue check (valid=${valid})`, async () => {
    const f = fixture({ issue: {
      body: valid ? completeBody : "Incomplete report",
      labels: [INVALID, "template-grace", "invalid-template-again", "triaged"],
    } });
    await enforce(f);
    assert.equal(f.issue.state, valid ? "open" : "closed");
    assert.deepEqual(f.labels().sort(), valid ? ["bug", "triaged"] : [INVALID, "triaged"]);
    assert.ok(f.calls.filter(call => call.method === "removeLabel").every(call => call.name !== "triaged"));
  });
}

test("a complete issue without its type label preserves maintainer triage", async () => {
  const f = fixture({ issue: { body: completeBody, labels: ["question"] } });
  await enforce(f);
  assert.equal(f.issue.state, "open");
  assert.deepEqual(f.labels(), ["question"]);
  assert.equal(f.comments.length, 0);
  assert.deepEqual(f.calls.map(call => call.method), ["get"]);
});

test("a recovered issue gets its matching label and an explanation", async () => {
  const f = fixture({ issue: { body: completeBody, labels: [INVALID] }, action: "reopened" });
  await enforce(f);
  assert.equal(f.issue.state, "open");
  assert.deepEqual(f.labels(), ["bug"]);
  assert.match(f.comments[0].body, /add the `bug` label/);
  assert.match(f.comments[0].body, /remove these labels: `invalid-template`/);
});

for (const labels of [["template-grace"], ["invalid-template-again"], ["template-grace", "invalid-template-again"]]) {
  for (const action of ["opened", "edited", "reopened"]) {
    test(`legacy cleanup preserves maintainer triage on ${action}: ${labels.join(", ")}`, async () => {
      const f = fixture({ action, issue: { body: completeBody, labels: [...labels, "triaged"] } });
      await enforce(f);
      assert.equal(f.issue.state, "open");
      assert.deepEqual(f.labels(), ["triaged"]);
      assert.ok(!f.calls.some(call => call.method === "addLabels"));
      assert.doesNotMatch(f.comments[0].body, /add the `bug` label/);
    });
  }
}

test("a missing title prefix closes with links to all four forms", async () => {
  const f = fixture({ issue: { title: "The app stops" } });
  await enforce(f);
  assert.equal(f.issue.state, "closed");
  for (const form of ["bug_report.yml", "feature_request.yml", "documentation.yml", "question.yml"]) {
    assert.ok(f.comments[0].body.includes("issues/new?template=" + form));
  }
});

test("enforcement reads the current body and state", async () => {
  const fixed = fixture({ live: { body: completeBody, labels: [INVALID] } });
  await enforce(fixed);
  assert.equal(fixed.issue.state, "open");
  assert.deepEqual(fixed.labels(), ["bug"]);
  const closed = fixture({ live: { state: "closed" } });
  await enforce(closed);
  assert.equal(closed.comments.length, 0);
  assert.deepEqual(closed.calls.map(call => call.method), ["get"]);
});

for (const action of ["opened", "edited", "reopened", "closed", "unlabeled"]) {
  test(`a closed issue receives no automatic work on ${action}`, async () => {
    const f = fixture({ issue: { state: "closed", labels: [INVALID, "template-grace", "invalid-template-again"] }, action });
    await enforce(f);
    assert.deepEqual(f.calls, []);
    assert.deepEqual(f.labels(), [INVALID, "template-grace", "invalid-template-again"]);
  });
}

for (const options of [
  { action: "unlabeled" }, { eventName: "issue_comment" }, { eventName: "schedule" },
  { eventName: "workflow_dispatch" }, { issue: { pull_request: {} } },
]) {
  test(`unrelated events do not check issues: ${JSON.stringify(options)}`, async () => {
    const f = fixture(options);
    await enforce(f);
    assert.deepEqual(f.calls, []);
  });
}

test("event checks keep pending events for the same issue in their queue", () => {
  const source = fs.readFileSync(path.join(root, ".github/workflows/issue-enforce.yml"), "utf8");
  assert.match(source, /group: issue-template-\$\{\{ github.event.issue.number \}\}/);
  assert.match(source, /queue: max\b/);
  assert.match(source, /cancel-in-progress: false\b/);
  assert.doesNotMatch(source, /^concurrency:/m, "Closed issue events must not enter the queue");
});

for (const method of ["listComments", "createComment", "createLabel", "addLabels", "removeLabel"]) {
  for (const change of ["correction", "closure", "label"]) {
    test(`a ${change} during ${method} stops the old decision`, async () => {
      const f = fixture({ issue: { labels: ["template-grace"] } });
      const original = f.github.rest.issues[method];
      let afterChange;
      f.github.rest.issues[method] = async args => {
        const result = await original(args);
        if (change === "correction") f.issue.body = completeBody;
        if (change === "closure") f.issue.state = "closed";
        if (change === "label") f.issue.labels.push({ name: "triaged" });
        afterChange = f.calls.length;
        return result;
      };
      await enforce(f);
      assert.notEqual(afterChange, undefined, "The API operation must run");
      assert.ok(f.calls.slice(afterChange).every(call => call.method === "get"));
      assert.ok(!f.calls.some(call => call.method === "update"));
    });
  }
}

test("an incomplete edit during a valid report comment stops label cleanup", async () => {
  const f = fixture({ issue: { body: completeBody, labels: [INVALID] } });
  const comment = f.github.rest.issues.createComment;
  f.github.rest.issues.createComment = async args => {
    await comment(args);
    f.issue.body = "Incomplete report";
  };
  await enforce(f);
  assert.equal(f.issue.state, "open");
  assert.deepEqual(f.labels(), [INVALID]);
});

test("an existing repository label does not prevent closure", async () => {
  const f = fixture();
  f.github.rest.issues.createLabel = async () => {
    throw Object.assign(new Error("Already exists"), { status: 422 });
  };
  await enforce(f);
  assert.equal(f.issue.state, "closed");
});

for (const method of ["createComment", "createLabel", "addLabels", "removeLabel"]) {
  test(`an API failure in ${method} leaves the issue open`, async () => {
    const f = fixture({ issue: { labels: ["template-grace"] } });
    f.github.rest.issues[method] = async () => { throw new Error("API failed"); };
    await assert.rejects(enforce(f), /API failed/);
    assert.equal(f.issue.state, "open");
    assert.ok(!f.calls.some(call => call.method === "update"));
  });
}

test("later event runs reuse the explanation after a partial closure fails", async () => {
  const f = fixture({ failUpdate: true });
  for (let attempt = 0; attempt < 3; attempt++) {
    f.event();
    await assert.rejects(enforce(f), /Update failed/);
  }
  assert.equal(f.comments.length, 1);
});

test("a changed invalid body gets a new explanation", async () => {
  const f = fixture({ failUpdate: true });
  await assert.rejects(enforce(f), /Update failed/);
  f.issue.body = completeBody.replace("macOS 15", "");
  f.event();
  await assert.rejects(enforce(f), /Update failed/);
  assert.equal(f.comments.length, 2);
  assert.match(f.comments[1].body, /### Environment/);
  assert.doesNotMatch(f.comments[1].body, /### Expected Behavior/);
});

test("each manual reopen gets a fresh explanation, but a rerun does not repeat it", async () => {
  const f = fixture();
  for (let cycle = 0; cycle < 3; cycle++) {
    f.issue.state = "open";
    f.event("reopened");
    await enforce(f);
    assert.equal(f.issue.state, "closed");
    assert.equal(f.comments.length, cycle + 1);
    await enforce(f);
    assert.equal(f.comments.length, cycle + 1);
  }
});

test("a successful check ends comment reuse before the same invalid body returns", async () => {
  const f = fixture();
  const invalidBody = f.issue.body;
  await enforce(f);
  f.issue.state = "open";
  f.issue.body = completeBody;
  f.event("reopened");
  await enforce(f);
  f.issue.body = invalidBody;
  f.event();
  await enforce(f);
  assert.equal(f.comments.length, 3);
});

test("closure posts an explanation before any label or state change", async () => {
  const f = fixture();
  await enforce(f);
  const explanation = f.calls.findIndex(call => call.method === "createComment");
  const action = f.calls.findIndex(call => ["addLabels", "removeLabel", "update"].includes(call.method));
  assert.ok(explanation >= 0 && explanation < action);
});

test("a user cannot suppress the bot explanation with a copied marker", async () => {
  const f = fixture({ failUpdate: true });
  await assert.rejects(enforce(f), /Update failed/);
  f.comments[0].user.type = "User";
  await assert.rejects(enforce(f), /Update failed/);
  assert.equal(f.comments.length, 2);
});

test("comment reuse reads past the first page", async () => {
  const f = fixture({ failUpdate: true, comments: Array.from({ length: 100 }, () => "Older comment") });
  await assert.rejects(enforce(f), /Update failed/);
  f.event();
  await assert.rejects(enforce(f), /Update failed/);
  assert.equal(f.comments.length, 101);
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
  for (const field of ["```text\n```", "```\n\n```", "~~~\n~~~", "```\n_No response_\n```", "- ```text\n  ```"]) {
    const result = validate({ title: "[BUG] Example", body: completeBody.replace("macOS 15", field) });
    assert.deepEqual(result.missing, ["Environment"], field);
  }
  assert.equal(validate({ title: "[BUG] Example", body: completeBody.replace("macOS 15", "```text\nmacOS 15\n```") }).ok, true);
  assert.equal(validate({ title: "No prefix", body: completeBody }).ok, false);
});

test("headings within completed form fields do not close an issue", async () => {
  for (const level of [1, 2, 3, 4, 5, 6]) {
    const body = completeBody.replace("### Bug Description\nThe app stops.", "### Bug Description\n" + "#".repeat(level) + " Details\nThe app stops.");
    const f = fixture({ issue: { body } });
    await enforce(f);
    assert.equal(f.issue.state, "open", "heading level " + level);
    assert.ok(!f.calls.some((call) => call.method === "update"));
  }
});

test("each form accepts headings in every required field", () => {
  const { TEMPLATES, validate } = require("./issue-template.js");
  for (const [prefix, form] of Object.entries(TEMPLATES)) {
    for (const level of [1, 2, 3]) {
      const body = form.required.map((name) => "### " + name + "\n" + "#".repeat(level) + " Summary\nSome details.").join("\n\n") +
        "\n\n### Checklist\n" + form.checklist.map((item) => "- [x] " + item).join("\n");
      assert.equal(validate({ title: prefix + " Example", body }).ok, true, prefix + " heading level " + level);
    }
  }
});

test("optional form fields cannot supply missing required information", () => {
  const { TEMPLATES, validate } = require("./issue-template.js");
  for (const [prefix, form] of Object.entries(TEMPLATES)) {
    for (const optional of form.optional) {
      const empty = form.required.at(-1);
      const body = form.required.map((name) => "### " + name + "\n" + (name === empty ? "" : "Some details.")).join("\n\n") +
        "\n\n### " + optional + "\nOptional details.\n\n### Checklist\n" + form.checklist.map((item) => "- [x] " + item).join("\n");
      assert.deepEqual(validate({ title: prefix + " Example", body }).missing, [empty], prefix + " optional field " + optional);
    }
  }
});

test("a field name from another form remains content", () => {
  const { validate } = require("./issue-template.js");
  const body = completeBody.replace("### Bug Description\nThe app stops.", "### Bug Description\n### What's Wrong\nThe app stops.");
  assert.equal(validate({ title: "[BUG] Example", body }).ok, true);
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

test("checked task items accept ordered lists and trailing notes", async () => {
  for (const [first, second] of [["1.", "2."], ["1)", "2)"], ["-", "-"], ["*", "*"], ["+", "+"]]) {
    for (const suffix of ["", " (see above)"]) {
      const body = completeBody.replace("- [x] I have described the issue clearly", first + " [x] I have described the issue clearly" + suffix)
        .replace("- [x] I have searched for similar issues", second + " [x] I have searched for similar issues" + suffix);
      const f = fixture({ issue: { body } });
      await enforce(f);
      assert.equal(f.issue.state, "open", first + suffix);
    }
  }
});

test("ordered task items in code, unchecked boxes, and changed required text still fail", () => {
  const { validate } = require("./issue-template.js");
  const ordered = completeBody.replace("- [x] I have described", "1. [x] I have described")
    .replace("- [x] I have searched", "2. [x] I have searched");
  const [fields, boxes] = ordered.split("### Checklist\n");
  for (const body of [
    fields + "### Checklist\n```text\n" + boxes + "\n```",
    fields + "### Checklist\n" + boxes.split("\n").map((line) => "    " + line).join("\n"),
    ordered.replace("1. [x]", "1. [ ]"),
    ordered.replace("issue clearly", "issue clearlyer"),
  ]) assert.equal(validate({ title: "[BUG] Test", body }).ok, false);
});

test("ordered markers in paragraph text or with too many digits are not task items", () => {
  const { validate } = require("./issue-template.js");
  for (const body of [
    completeBody.replace("### Checklist\n", "### Checklist\nNotes.\n")
      .replace("- [x] I have described", "2. [x] I have described").replace("- [x] I have searched", "3. [x] I have searched"),
    completeBody.replaceAll("- [x]", "1000000000. [x]"),
  ]) assert.equal(validate({ title: "[BUG] Test", body }).ok, false);
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
    const f = fixture({ issue: { body, labels: ["bug"] } });
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
    const f = fixture({ issue: { body, labels: ["bug"] } });
    await enforce(f);
    assert.equal(f.issue.state, "open", example);
    assert.deepEqual(f.labels(), ["bug"]);
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

test("quotes inside lists preserve the container for later fences", async () => {
  for (const example of [
    "- item\n  > quote\n  ```text\n  code",
    "1. item\n   > quote\n   ```text\n   code",
    "- outer\n  - item\n    > quote\n    ```text\n    code",
    "- item\n  > quote\n    ```text\n    code",
  ]) {
    const body = completeBody.replace("The app stops.", "The app stops.\n\n" + example);
    const f = fixture({ issue: { body } });
    await enforce(f);
    assert.equal(f.issue.state, "open", example);
  }
});

test("a quote does not let an outdented fence close a list-item code block", () => {
  const { validate } = require("./issue-template.js");
  for (const example of [
    "- item\n  > quote\n  ```text\n  code\n```",
    "- outer\n  - item\n    > quote\n    ```text\n    code\n```",
    "- item\n> root quote\n  ```text\n  code",
  ]) {
    const body = completeBody.replace("The app stops.", "The app stops.\n\n" + example);
    assert.equal(validate({ title: "[BUG] Test", body }).ok, false, example);
  }
});

test("HTML blocks after GFM tables cannot supply required fields", async () => {
  for (const example of [
    "| a | b |\n| - | - |\n| 1 | 2 |",
    "a | b\n--|--\n1|2",
    "| a | b |\n| :-- | --: |\n| 1 | 2 |",
    "a\n| - |\n1",
    "a\n:-\n1",
    "| a | b |\n| - | - |\nplain row",
    "paragraph\na | b\n--|--\n1|2",
  ]) {
    const body = completeBody.replace("The app stops.", "The app stops.\n\n" + example + "\n<span>");
    const f = fixture({ issue: { body } });
    await enforce(f);
    assert.equal(f.issue.state, "closed", example);
    assert.match(f.comments[0].body, /### Current Behavior/);
  }
});

test("table lookalikes and new paragraphs keep inline HTML within their paragraph", () => {
  const { validate } = require("./issue-template.js");
  for (const example of [
    "| a | b |\n| - |\n| 1 |",
    "| a | b |\n| bad | --- |\n| 1 | 2 |",
    "a\\|b\n--|--\n1|2",
    "| a |\n---\n1",
    "| a | b |\n| - | - |\n| 1 | 2 |\n\nA new paragraph.",
  ]) {
    const body = completeBody.replace("The app stops.", "The app stops.\n\n" + example + "\n<span>");
    assert.equal(validate({ title: "[BUG] Test", body }).ok, true, example);
  }
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
