'use strict';

const fs = require('node:fs');

const DOC_AND_META_FILES = new Set([
  'AGENTS.md',
  'LICENSE',
  '.github/pull_request_template.md',
  '.github/scripts/issue-template.js',
  '.github/scripts/issue-workflows.js',
  '.github/scripts/issue-workflows.test.cjs',
  '.github/workflows/issue-enforce.yml',
  '.github/workflows/issue-grace.yml',
  '.github/workflows/opencode-review.yml',
]);

// Files under frontend/ that affect native/Rust builds or tests (e.g. include_str! in Rust).
const FRONTEND_NATIVE_PATHS = new Set([
  'frontend/scripts/build-diarization-sidecar.js',
  'frontend/scripts/build-funasr-sidecar.js',
  'frontend/src/lib/speaker-colors.json',
]);

function isDocOrMeta(filePath) {
  if (DOC_AND_META_FILES.has(filePath)) return true;
  if (/^LICENSE(?:-.+)?$/i.test(filePath)) return true;
  if (filePath.startsWith('docs/')) return true;
  if (filePath.startsWith('.github/ISSUE_TEMPLATE/')) return true;
  if (/\.md$/i.test(filePath)) {
    return !filePath.includes('/') || filePath.startsWith('.github/') || /^[^/]+\/README\.md$/i.test(filePath);
  }
  return false;
}

function isFrontendOnly(filePath) {
  return (
    filePath.startsWith('frontend/') &&
    !filePath.startsWith('frontend/src-tauri/') &&
    !filePath.startsWith('frontend/e2e/native/') &&
    !FRONTEND_NATIVE_PATHS.has(filePath)
  );
}

/**
 * Conservatively classifies changed repository paths into 'docs', 'frontend', or 'all'.
 * Any empty list or unrecognized path fails open with 'all'.
 */
function classifyChangedFiles(files) {
  const paths = (files || []).filter((p) => typeof p === 'string' && p.length > 0);
  if (paths.length === 0) return 'all';

  let sawFrontend = false;
  for (const file of paths) {
    if (isDocOrMeta(file)) continue;
    if (isFrontendOnly(file)) {
      sawFrontend = true;
      continue;
    }
    return 'all';
  }
  return sawFrontend ? 'frontend' : 'docs';
}

if (require.main === module) {
  const raw = fs.readFileSync(0, 'utf8');
  process.stdout.write(`${classifyChangedFiles(raw.split('\0'))}\n`);
}

module.exports = { classifyChangedFiles };
