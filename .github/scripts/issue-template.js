"use strict";

// Shared validation for the issue templates in .github/ISSUE_TEMPLATE/.
// Used by event checks and the nightly check through issue-workflows.js.
// Keep this file dependency-free so the workflows can load it after checkout.

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

// Issue forms use level-three headings. Deeper headings remain field content.
// Keep code in text fields, but exclude it when checking task-list boxes.
function parseSections(markdown, { includeCode = true } = {}) {
  const sections = new Map();
  let current = null;
  let fence = null;
  for (const line of markdown.split(/\r?\n/)) {
    const marker = line.match(/^ {0,3}(`{3,}|~{3,})(.*)$/);
    if (fence) {
      if (includeCode && current) sections.get(current).push(line);
      if (marker && marker[1][0] === fence.char && marker[1].length >= fence.length && !marker[2].trim()) {
        fence = null;
      }
      continue;
    }
    if (marker && (marker[1][0] === "~" || !marker[2].includes("`"))) {
      fence = { char: marker[1][0], length: marker[1].length };
      if (includeCode && current) sections.get(current).push(line);
      continue;
    }
    const heading = line.match(/^ {0,3}###[ \t]+(.+?)[ \t]*$/);
    if (heading) {
      current = heading[1].replace(/[ \t]+#+[ \t]*$/, "").trim();
      if (!sections.has(current)) sections.set(current, []);
      continue;
    }
    if (current) sections.get(current).push(line);
  }
  return sections;
}

function findSection(sections, heading) {
  const match = Array.from(sections.keys()).find(function (key) {
    return key.toLowerCase() === heading.toLowerCase();
  });
  return match === undefined ? null : sections.get(match).join("\n");
}

function isBlankSection(content) {
  return content === null || content.trim() === "" || /^_no response_$/i.test(content.trim());
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
      "^ {0,3}[-*+][ \\t]+\\[[xX]\\][ \\t]+" + escapeRegExp(item) + "[ \\t]*$",
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
  const unchecked = uncheckedCheckboxes(template, parseSections(issue.body || "", { includeCode: false }));
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
