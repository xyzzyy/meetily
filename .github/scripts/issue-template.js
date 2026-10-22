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

// HTML block rules: https://spec.commonmark.org/0.31.2/#html-blocks.
// A null end marker means the block ends at the next blank line.
function htmlBlockStart(line, inParagraph) {
  if (/^ {0,3}<(?:script|pre|style|textarea)(?:\s|>|$)/i.test(line)) {
    return { end: /<\/(?:script|pre|style|textarea)>/i, hidden: /^ {0,3}<(?:script|style)(?:\s|>|$)/i.test(line) };
  }
  for (const [start, end] of [
    [/^ {0,3}<!--/, /-->/],
    [/^ {0,3}<\?/, /\?>/],
    [/^ {0,3}<![A-Z]/, />/],
    [/^ {0,3}<!\[CDATA\[/, /\]\]>/],
  ]) {
    if (start.test(line)) return { end, hidden: true };
  }
  if (/^ {0,3}<\/?(?:address|article|aside|base|basefont|blockquote|body|caption|center|col|colgroup|dd|details|dialog|dir|div|dl|dt|fieldset|figcaption|figure|footer|form|frame|frameset|h[1-6]|head|header|hr|html|iframe|legend|li|link|main|menu|menuitem|nav|noframes|ol|optgroup|option|p|param|section|source|summary|table|tbody|td|tfoot|th|thead|title|tr|track|ul)(?:\s|\/?>|$)/i.test(line)) {
    return { end: null, hidden: false };
  }
  // A complete tag on its own line starts a block only outside a paragraph.
  if (!inParagraph && /^ {0,3}(?:<\/[a-z][a-z0-9-]*[ \t]*>|<[a-z][a-z0-9-]*(?:[ \t]+[a-z_:][a-z0-9_.:-]*(?:[ \t]*=[ \t]*(?:[^ \t"'=<>`]+|'[^']*'|"[^"]*"))?)*[ \t]*\/?>)[ \t]*$/i.test(line)) {
    return { end: null, hidden: false };
  }
  return null;
}

// Issue forms use level-three headings. Accept the older level-one and
// level-two sections too. Deeper headings remain field content.
// Keep code in text fields, but exclude it when checking task-list boxes.
function parseSections(markdown, { includeCode = true } = {}) {
  const lines = markdown.split(/\r?\n/);
  const sections = new Map();
  let current = null;
  let fence = null;
  let html = null;
  let inParagraph = false;
  let inComment = false;
  const thematicBreak = (line) => /^ {0,3}(?:(?:\*[ \t]*){3,}|(?:_[ \t]*){3,}|(?:-[ \t]*){3,})$/.test(line);
  function inlineCommentCloses(line, offset, lineIndex) {
    if (line.includes("-->", offset)) return true;
    // Inline comments must close inside their paragraph. Block comments are
    // handled by htmlBlockStart and can continue across Markdown blocks.
    for (let index = lineIndex + 1; index < lines.length; index++) {
      const next = lines[index];
      if (!next.trim() || /^ {0,3}(?:#{1,6}(?:[ \t]+|$)|`{3,}|~{3,}|>|[-+*][ \t]+|1[.)][ \t]+)/.test(next) ||
          thematicBreak(next) || /^ {0,3}(?:=+|-+)[ \t]*$/.test(next) || htmlBlockStart(next, true)) return false;
      if (next.includes("-->")) return true;
    }
    return false;
  }
  function withoutComments(line, lineIndex) {
    let visible = "";
    let offset = 0;
    while (offset < line.length) {
      if (inComment) {
        const end = line.indexOf("-->", offset);
        if (end === -1) return visible;
        // Keep a comment's suffix from becoming a heading or task-list marker.
        visible += " ".repeat(Math.max(4, end + 3 - offset));
        offset = end + 3;
        inComment = false;
      } else {
        const tokens = /`+|<!--/g;
        tokens.lastIndex = offset;
        const token = tokens.exec(line);
        if (!token) return visible + line.slice(offset);
        visible += line.slice(offset, token.index);
        offset = token.index + token[0].length;
        let escapes = 0;
        for (let index = token.index - 1; index >= 0 && line[index] === "\\"; index--) escapes++;
        if (escapes % 2) {
          visible += token[0];
        } else if (token[0] === "<!--") {
          if (inlineCommentCloses(line, offset, lineIndex)) inComment = true;
          else visible += token[0];
        } else {
          // Comment syntax inside an inline code span is literal text.
          const ticks = /`+/g;
          ticks.lastIndex = offset;
          let close;
          while ((close = ticks.exec(line)) && close[0].length !== token[0].length) {}
          const end = close ? close.index + close[0].length : offset;
          visible += line.slice(token.index, end);
          offset = end;
        }
      }
    }
    return visible;
  }
  for (const [lineIndex, rawLine] of lines.entries()) {
    if (html) {
      if (includeCode && current && !html.hidden) sections.get(current).push(rawLine);
      if (html.end ? html.end.test(rawLine) : !rawLine.trim()) html = null;
      continue;
    }
    const marker = rawLine.match(/^ {0,3}(`{3,}|~{3,})(.*)$/);
    if (fence) {
      if (includeCode && current) sections.get(current).push(rawLine);
      if (marker && marker[1][0] === fence.char && marker[1].length >= fence.length && !marker[2].trim()) {
        fence = null;
      }
      continue;
    }
    // Indented code cannot interrupt a paragraph. Outside a paragraph its
    // comment markers are literal, just as they are inside a fenced block.
    if (!inComment && !inParagraph && /^(?: {4}| {0,3}\t)/.test(rawLine)) {
      if (includeCode && current) sections.get(current).push(rawLine);
      continue;
    }
    if (!inComment && marker && (marker[1][0] === "~" || !marker[2].includes("`"))) {
      fence = { char: marker[1][0], length: marker[1].length };
      inParagraph = false;
      if (includeCode && current) sections.get(current).push(rawLine);
      continue;
    }
    if (!inComment) {
      const start = htmlBlockStart(rawLine, inParagraph);
      if (start) {
        if (includeCode && current && !start.hidden) sections.get(current).push(rawLine);
        if (!start.end || !start.end.test(rawLine)) html = start;
        inParagraph = false;
        continue;
      }
    }
    const line = withoutComments(rawLine, lineIndex);
    const heading = line.match(/^ {0,3}#{1,3}[ \t]+(.+?)[ \t]*$/);
    if (heading) {
      current = heading[1].replace(/[ \t]+#+[ \t]*$/, "").trim();
      if (!sections.has(current)) sections.set(current, []);
      inParagraph = false;
      continue;
    }
    if (current) sections.get(current).push(line);
    const setextUnderline = inParagraph && /^ {0,3}(?:=+|-+)[ \t]*$/.test(line);
    inParagraph = !!line.trim() && !thematicBreak(line) && !setextUnderline &&
      !/^ {0,3}#{4,6}(?:[ \t]+|$)/.test(line);
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
