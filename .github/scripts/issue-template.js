"use strict";

// Shared validation for the issue templates in .github/ISSUE_TEMPLATE/.
// Used by issue-enforce.yml (warn + close) and issue-grace.yml (author reopen +
// nightly expiry). Keep this file dependency-free so both workflows can load it
// after actions/checkout.

const TEMPLATES = {
  "[bug]": {
    name: "Bug Report",
    file: "bug_report.yml",
    label: "bug",
    required: [
      "Bug Description",
      "Current Behavior",
      "Expected Behavior",
      "Steps to Reproduce",
      "Environment",
    ],
    checklist: [
      "I have described the issue clearly",
      "I have searched for similar issues",
    ],
  },
  "[feature]": {
    name: "Feature Request",
    file: "feature_request.yml",
    label: "enhancement",
    required: ["Feature Description", "Upstream Work & Prior Art"],
    checklist: [
      "I have described the issue clearly",
      "I have searched for similar feature requests",
    ],
  },
  "[docs]": {
    name: "Documentation",
    file: "documentation.yml",
    label: "documentation",
    required: ["Documentation Location", "Issue Type", "What's Wrong"],
    checklist: [
      "I have described the issue clearly",
      "I have searched for similar documentation issues",
    ],
  },
  "[question]": {
    name: "Question",
    file: "question.yml",
    label: "question",
    required: ["What are you trying to do?", "What have you already tried?"],
    checklist: ["I have described the issue clearly"],
  },
};

function detectTemplate(title) {
  const lower = (title || "").toLowerCase();
  for (const prefix of Object.keys(TEMPLATES)) {
    if (lower.startsWith(prefix)) return TEMPLATES[prefix];
  }
  return null;
}

// Split the body into its `###` sections, ignoring headings that appear inside
// fenced code blocks (logs often contain them).
function parseSections(markdown) {
  const sections = new Map();
  let current = null;
  let fenceChar = null;
  for (const line of markdown.split(/\r?\n/)) {
    const fence = line.match(/^\s*(```+|~~~+)/);
    if (fence) {
      if (!fenceChar) {
        fenceChar = fence[1][0];
      } else if (fence[1][0] === fenceChar) {
        fenceChar = null;
      }
      if (current) sections.get(current).push(line);
      continue;
    }
    if (!fenceChar) {
      const heading = line.match(/^#{1,6}[ \t]+(.+?)[ \t]*$/);
      if (heading) {
        current = heading[1].trim();
        if (!sections.has(current)) sections.set(current, []);
        continue;
      }
    }
    if (current) sections.get(current).push(line);
  }
  return sections;
}

function findSection(sections, heading) {
  const match = Array.from(sections.keys()).find(function (key) {
    return key.toLowerCase() === heading.toLowerCase();
  });
  return match === undefined ? null : sections.get(match).join("\n").trim();
}

function isBlankSection(content) {
  return content === null || content === "" || /^_no response_$/i.test(content);
}

function escapeRegExp(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function missingSections(template, sections) {
  const missing = [];
  for (const heading of template.required) {
    if (isBlankSection(findSection(sections, heading))) missing.push(heading);
  }
  return missing;
}

function uncheckedCheckboxes(template, sections) {
  const checklist = findSection(sections, "Checklist");
  if (checklist === null) return template.checklist.slice();
  const unchecked = [];
  for (const item of template.checklist) {
    const pattern = new RegExp(
      "^[ \\t]*[-*+][ \\t]*\\[[xX]\\][ \\t]*" + escapeRegExp(item) + "[ \\t]*$",
      "m"
    );
    if (!pattern.test(checklist)) unchecked.push(item);
  }
  return unchecked;
}

// Returns { ok, template, missing, unchecked }. `template` is null when the
// title does not carry a recognized prefix.
function validate(issue) {
  const template = detectTemplate(issue.title);
  if (!template) {
    return { ok: false, template: null, missing: [], unchecked: [] };
  }
  const sections = parseSections(issue.body || "");
  const missing = missingSections(template, sections);
  const unchecked = uncheckedCheckboxes(template, sections);
  return {
    ok: missing.length === 0 && unchecked.length === 0,
    template: template,
    missing: missing,
    unchecked: unchecked,
  };
}

function newIssueUrl(owner, repo, template) {
  return (
    "https://github.com/" +
    owner +
    "/" +
    repo +
    "/issues/new?template=" +
    template.file
  );
}

module.exports = {
  TEMPLATES,
  detectTemplate,
  parseSections,
  validate,
  newIssueUrl,
};
