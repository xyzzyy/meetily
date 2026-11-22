const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { resolve } = require('node:path');
const { test } = require('node:test');

const root = resolve(__dirname, '..');
const workflowPath = resolve(root, '.github/workflows/pr-build.yml');
const packageJsonPath = resolve(root, 'frontend/package.json');

const browserJobs = ['browser-tests', 'browser-walkthrough'];

function readPlaywrightVersion() {
  const pkg = JSON.parse(readFileSync(packageJsonPath, 'utf8'));
  const version = pkg.devDependencies?.['@playwright/test'];
  assert.ok(version, 'frontend/package.json must declare devDependencies["@playwright/test"]');
  assert.match(
    version,
    /^\d+\.\d+\.\d+$/,
    `@playwright/test must be pinned to an exact version so the Playwright Docker image tag stays in sync (got: ${version})`
  );
  return version;
}

function getJobBlock(workflow, jobName) {
  const match = workflow.match(
    new RegExp(`^  ${jobName}:\\n([\\s\\S]*?)(?=^  [\\w-]+:|$(?![\\s\\S]))`, 'm')
  );
  assert.ok(match, `Missing job "${jobName}" in .github/workflows/pr-build.yml`);
  return match[1];
}

test('browser CI jobs use the Playwright container image matching frontend/package.json', () => {
  const expectedVersion = readPlaywrightVersion();
  const workflow = readFileSync(workflowPath, 'utf8');

  for (const jobName of browserJobs) {
    const block = getJobBlock(workflow, jobName);
    const containerMatch = block.match(/^    container:\n((?:      .*\n?)*)/m);
    assert.ok(containerMatch, `Job "${jobName}" must define a container block`);

    const containerBlock = containerMatch[1];
    const imageMatch = containerBlock.match(
      /^      image:\s*['"]?mcr\.microsoft\.com\/playwright:v(\d+\.\d+\.\d+)-([a-z0-9.-]+)['"]?\s*$/m
    );
    assert.ok(
      imageMatch,
      `Job "${jobName}" must use mcr.microsoft.com/playwright:vX.Y.Z-<distro>`
    );
    assert.equal(
      imageMatch[1],
      expectedVersion,
      `Job "${jobName}" Playwright image version v${imageMatch[1]} does not match @playwright/test ${expectedVersion} in frontend/package.json`
    );

    const optionsMatch = containerBlock.match(/^      options:\s*(.+)$/m);
    assert.ok(optionsMatch, `Job "${jobName}" container must set options`);
    assert.match(
      optionsMatch[1],
      /(?:^|\s)--ipc=host(?:\s|$)/,
      `Job "${jobName}" container options must include --ipc=host`
    );

    assert.doesNotMatch(
      block,
      /playwright\s+install/,
      `Job "${jobName}" should use preinstalled container browsers instead of running playwright install`
    );
  }
});

test('all Playwright container image tags in pr-build.yml match @playwright/test', () => {
  const expectedVersion = readPlaywrightVersion();
  const workflow = readFileSync(workflowPath, 'utf8');
  const matches = [...workflow.matchAll(/mcr\.microsoft\.com\/playwright:v(\d+\.\d+\.\d+)-[^\s'"]+/g)];
  assert.ok(matches.length >= browserJobs.length, 'Expected Playwright container image references in pr-build.yml');
  for (const match of matches) {
    assert.equal(
      match[1],
      expectedVersion,
      `Playwright image "${match[0]}" does not match @playwright/test ${expectedVersion} in frontend/package.json`
    );
  }
});
