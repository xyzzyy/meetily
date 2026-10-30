const { test } = require('node:test');
const assert = require('node:assert/strict');
const { jobs, select, changedPaths } = require('./select-pr-checks.cjs');
const selected = paths => Object.keys(select(paths)).filter(job => select(paths)[job]);
test('docs and issue templates keep quick checks without app builds', () => {
  assert.deepEqual(selected(['README.md', 'docs/setup.md', '.github/ISSUE_TEMPLATE/bug_report.yml']), ['node-quick']);
});
test('benchmark changes select benchmark tests', () => {
  assert.deepEqual(selected(['diarization-sidecar/Benchmarks/score.py']), ['node-quick', 'benchmark-tests']);
});
test('frontend unit tests do not build the app', () => {
  assert.deepEqual(selected(['frontend/tests/components/example.test.mjs']), ['node-quick', 'frontend-tests']);
});
test('UI changes build their browser input but do not compile Rust', () => {
  const checks = select(['frontend/src/components/Settings.tsx']);
  for (const job of ['frontend-tests', 'frontend-build', 'browser-tests', 'browser-walkthrough']) assert.equal(checks[job], true);
  assert.equal(checks['rust-tests'], false);
  assert.equal(checks['macos-app'], false);
});
test('native checks include all required artifacts', () => {
  const checks = select(['frontend/src-tauri/src/lib.rs']);
  for (const job of ['frontend-build', 'native-helpers', 'rust-tests', 'macos-app', 'app-walkthrough']) assert.equal(checks[job], true);
  assert.equal(checks['frontend-tests'], false);
});
for (const [path, job] of [['llama-helper/src/main.rs', 'llama-tests'], ['funasr-helper/src/main.cpp', 'funasr-tests'], ['diarization-sidecar/Package.swift', 'swift-tests']]) {
  test(`${path} selects its helper tests and app integration`, () => {
    const checks = select([path]);
    assert.equal(checks[job], true);
    assert.equal(checks['native-helpers'], true);
    assert.equal(checks['macos-app'], true);
  });
}
test('unknown files, dependency files, and workflow edits select everything', () => {
  for (const path of ['new-engine/source.rs', 'frontend/package.json', 'Cargo.lock', '.github/workflows/pr-build.yml', '.github/scripts/select-pr-checks.cjs']) {
    assert.ok(Object.values(select([path])).every(Boolean), path);
  }
  assert.ok(Object.values(select([], true)).every(Boolean));
});
test('mixed changes take the union', () => {
  const checks = select(['frontend/tests/one.test.mjs', 'diarization-sidecar/Benchmarks/score.py']);
  assert.equal(checks['frontend-tests'], true); assert.equal(checks['benchmark-tests'], true);
  assert.equal(checks['frontend-build'], false);
});
test('diff uses PR merge base, NUL paths, and both sides of moves', () => {
  const base = 'a'.repeat(40), head = 'b'.repeat(40);
  const paths = changedPaths({ pull_request: { base: { sha: base }, head: { sha: head } } }, args => {
    assert.deepEqual(args, ['diff', '--name-only', '--no-renames', '-z', `${base}...${head}`, '--']);
    return 'docs/old.md\0frontend/src/new name\nwith newline.tsx\0';
  });
  assert.equal(paths.length, 2); assert.equal(select(paths)['frontend-build'], true);
  assert.throws(() => changedPaths({}), /Missing PR commit hashes/);
});
const { verify } = require('./verify-pr-checks.cjs');
test('the final check rejects failed, cancelled, or skipped selected jobs', () => {
  const checks = select(['frontend/src/page.tsx']);
  const needs = Object.fromEntries(jobs.map(job => [job, { result: checks[job] ? 'success' : 'skipped' }]));
  needs.changes = { result: 'success', outputs: { checks: JSON.stringify(checks) } };
  assert.doesNotThrow(() => verify(needs));
  for (const result of ['failure', 'cancelled', 'skipped', undefined]) {
    needs['frontend-build'].result = result;
    assert.throws(() => verify(needs), /frontend-build/);
  }
  needs.changes.result = 'failure';
  assert.throws(() => verify(needs), /selection did not succeed/);
});
test('the workflow covers every selected job and reports a final result', () => {
  const fs = require('node:fs');
  const yaml = fs.readFileSync(require('node:path').join(__dirname, '../workflows/pr-build.yml'), 'utf8');
  for (const job of jobs) {
    assert.ok(yaml.includes(`fromJSON(needs.changes.outputs.checks)['${job}']`), job);
    const block = yaml.split(`\n  ${job}:\n`)[1]?.split(/\n  [a-z][a-z-]*:\n/)[0];
    assert.ok(block?.includes('needs:') && block.includes('changes'), job);
  }
  assert.match(yaml, /pr-checks:\n    name: PR checks\n    if: always\(\)/);
});
test('new workflow jobs cannot bypass selection or the final result', () => {
  const fs = require('node:fs');
  const yaml = fs.readFileSync(require('node:path').join(__dirname, '../workflows/pr-build.yml'), 'utf8');
  const ids = [...yaml.split('\njobs:\n')[1].matchAll(/^  ([a-z][a-z-]*):$/gm)].map(match => match[1]);
  assert.deepEqual(ids.sort(), ['changes', 'pr-checks', ...jobs].sort());
  const gate = yaml.split('\n  pr-checks:\n')[1];
  for (const job of ['changes', ...jobs]) assert.ok(gate.includes(job), job);
});
test('real git diff includes deleted source and renamed destinations beyond 300 paths', () => {
  const fs = require('node:fs'), os = require('node:os'), path = require('node:path');
  const { execFileSync } = require('node:child_process');
  const cwd = fs.mkdtempSync(path.join(os.tmpdir(), 'meetily-ci-diff-'));
  const git = args => execFileSync('git', args, { cwd, encoding: 'utf8' });
  try {
    git(['init', '-q']); git(['config', 'user.name', 'CI test']); git(['config', 'user.email', 'ci@example.invalid']);
    git(['config', 'commit.gpgsign', 'false']);
    fs.mkdirSync(path.join(cwd, 'frontend/src'), { recursive: true });
    fs.writeFileSync(path.join(cwd, 'frontend/src/deleted.ts'), 'export const value = 1;\n');
    git(['add', '.']); git(['commit', '-qm', 'base']); const base = git(['rev-parse', 'HEAD']).trim();
    fs.mkdirSync(path.join(cwd, 'docs'));
    fs.renameSync(path.join(cwd, 'frontend/src/deleted.ts'), path.join(cwd, 'docs/moved.md'));
    for (let i = 0; i < 350; i++) fs.writeFileSync(path.join(cwd, `docs/${i}.md`), 'doc');
    git(['add', '-A']); git(['commit', '-qm', 'changes']); const head = git(['rev-parse', 'HEAD']).trim();
    const paths = changedPaths({ pull_request: { base: { sha: base }, head: { sha: head } } }, git);
    assert.equal(paths.length, 352);
    assert.ok(paths.includes('frontend/src/deleted.ts'));
    assert.ok(paths.includes('docs/moved.md'));
    assert.equal(select(paths)['frontend-build'], true);
  } finally { fs.rmSync(cwd, { recursive: true, force: true }); }
});
test('CLI selects all checks when diff fails and for manual or main runs', () => {
  const fs = require('node:fs'), os = require('node:os'), path = require('node:path');
  const { spawnSync } = require('node:child_process');
  const cwd = fs.mkdtempSync(path.join(os.tmpdir(), 'meetily-ci-cli-'));
  const script = path.join(__dirname, 'select-pr-checks.cjs');
  try {
    const event = path.join(cwd, 'event.json'), output = path.join(cwd, 'output');
    fs.writeFileSync(event, JSON.stringify({ pull_request: { base: { sha: 'a'.repeat(40) }, head: { sha: 'b'.repeat(40) } } }));
    for (const name of ['pull_request', 'workflow_dispatch', 'push']) {
      fs.writeFileSync(output, '');
      const result = spawnSync(process.execPath, [script], { cwd, encoding: 'utf8', env: { ...process.env, GITHUB_EVENT_NAME: name, GITHUB_EVENT_PATH: event, GITHUB_OUTPUT: output } });
      assert.equal(result.status, 0, result.stderr);
      const checks = JSON.parse(fs.readFileSync(output, 'utf8').trim().slice('checks='.length));
      assert.ok(Object.values(checks).every(Boolean));
    }
  } finally { fs.rmSync(cwd, { recursive: true, force: true }); }
});
