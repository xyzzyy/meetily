'use strict';

const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const { mkdirSync, mkdtempSync, rmSync, writeFileSync } = require('node:fs');
const { tmpdir } = require('node:os');
const { join } = require('node:path');
const { test } = require('node:test');

const { classifyChangedFiles } = require('./pr-scope.cjs');

const CASES = [
  ['empty list fails open', [], 'all'],
  ['docs, root markdown, and component README', ['docs/ci.md', 'README.md', 'AGENTS.md', 'frontend/README.md'], 'docs'],
  ['issue workflow and template files', ['.github/ISSUE_TEMPLATE/bug_report.yml', '.github/workflows/issue-enforce.yml', '.github/workflows/WORKFLOWS_OVERVIEW.md', 'LICENSE'], 'docs'],
  ['frontend-only changes', ['frontend/src/app/page.tsx', 'frontend/e2e/meeting-details.spec.ts', 'frontend/package.json'], 'frontend'],
  ['mixed docs and frontend', ['docs/ci.md', 'frontend/src/components/App.tsx'], 'frontend'],
  ['markdown inside frontend/public or frontend/src', ['frontend/public/release-notes.md', 'frontend/src/content/help.md'], 'frontend'],
  ['speaker-colors.json embedded by Rust colors.rs', ['frontend/src/lib/speaker-colors.json'], 'all'],
  ['sidecar build script under frontend/scripts', ['frontend/scripts/build-funasr-sidecar.js'], 'all'],
  ['frontend/src-tauri or native e2e', ['frontend/src-tauri/src/lib.rs'], 'all'],
  ['markdown inside frontend/src-tauri', ['frontend/src-tauri/templates/prompt.md'], 'all'],
  ['native helper, Cargo, or workflow changes', ['.github/workflows/pr-build.yml'], 'all'],
  ['literal backslash filename is not rewritten to docs/', ['docs\\notes.txt'], 'all'],
  ['leading whitespace is preserved and fails open', [' frontend/src/app/page.tsx'], 'all'],
  ['native file moved into docs (--no-renames reports both)', ['llama-helper/src/main.rs', 'docs/main.rs'], 'all'],
];

for (const [name, files, expected] of CASES) {
  test(name, () => {
    assert.equal(classifyChangedFiles(files), expected);
  });
}

test('CLI reads NUL-delimited paths from stdin and handles git mv into docs', (t) => {
  const cli = join(__dirname, 'pr-scope.cjs');
  const direct = spawnSync(process.execPath, [cli], {
    input: 'docs/ spaced.md \0frontend/src/app/page.tsx\0',
    encoding: 'utf8',
  });
  assert.equal(direct.status, 0, direct.stderr);
  assert.equal(direct.stdout, 'frontend\n');

  const repo = mkdtempSync(join(tmpdir(), 'pr-scope-'));
  t.after(() => rmSync(repo, { recursive: true, force: true }));
  const git = (...args) => {
    const res = spawnSync(
      'git',
      ['-c', 'user.name=test', '-c', 'user.email=test@example.invalid', '-c', 'commit.gpgsign=false', '-c', 'core.hooksPath=/dev/null', ...args],
      { cwd: repo, encoding: 'buffer' }
    );
    assert.equal(res.status, 0, res.stderr.toString());
    return res.stdout;
  };

  git('init', '-q');
  mkdirSync(join(repo, 'llama-helper', 'src'), { recursive: true });
  writeFileSync(join(repo, 'llama-helper', 'src', 'main.rs'), 'fn main() {}\n');
  git('add', '.');
  git('commit', '-q', '-m', 'base');
  mkdirSync(join(repo, 'docs'));
  git('mv', 'llama-helper/src/main.rs', 'docs/main.rs');
  git('commit', '-q', '-m', 'move');

  const changed = git('diff', '--no-renames', '--name-only', '-z', 'HEAD^1', 'HEAD');
  const moved = spawnSync(process.execPath, [cli], { input: changed, encoding: 'utf8' });
  assert.equal(moved.status, 0, moved.stderr);
  assert.equal(moved.stdout, 'all\n');
});
