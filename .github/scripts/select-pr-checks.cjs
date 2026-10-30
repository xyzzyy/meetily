const { execFileSync } = require('node:child_process');
const fs = require('node:fs');

const jobs = ['node-quick', 'benchmark-tests', 'funasr-tests', 'swift-tests',
  'frontend-tests', 'frontend-build', 'browser-tests', 'native-helpers',
  'rust-tests', 'llama-tests', 'macos-app', 'app-walkthrough', 'browser-walkthrough'];
function select(paths, full = false) {
  const checks = Object.fromEntries(jobs.map(job => [job, full]));
  checks['node-quick'] = true;
  const enable = (...names) => names.forEach(name => { checks[name] = true; });
  const all = () => jobs.forEach(job => { checks[job] = true; });
  const frontend = () => enable('frontend-tests', 'frontend-build', 'browser-tests', 'browser-walkthrough');
  const native = () => enable('frontend-build', 'native-helpers', 'rust-tests', 'macos-app', 'app-walkthrough');
  for (const path of paths) {
    if (/^(README[^/]*\.md|AGENTS\.md|LICENSE(?:\.md|\.txt)?|docs\/.*|\.github\/ISSUE_TEMPLATE\/.*|\.github\/pull_request_template\.md)$/.test(path)) continue;
    if (path.startsWith('diarization-sidecar/Benchmarks/')) enable('benchmark-tests');
    else if (path.startsWith('frontend/tests/')) enable('frontend-tests');
    else if (path.startsWith('frontend/e2e/') || /^frontend\/playwright\./.test(path)) enable('frontend-build', 'browser-tests', 'browser-walkthrough');
    else if (path.startsWith('frontend/src/')) frontend();
    else if (path.startsWith('frontend/src-tauri/')) native();
    else if (path.startsWith('llama-helper/')) { native(); enable('llama-tests'); }
    else if (path.startsWith('funasr-helper/')) { native(); enable('funasr-tests'); }
    else if (path.startsWith('diarization-sidecar/')) { native(); enable('swift-tests', 'benchmark-tests'); }
    else all(); // New paths and build/CI/dependency changes require the full run.
  }
  return checks;
}
function changedPaths(event, git = args => execFileSync('git', args, { encoding: 'utf8', maxBuffer: 32 * 1024 * 1024 })) {
  const base = event.pull_request?.base?.sha;
  const head = event.pull_request?.head?.sha;
  if (!/^[a-f0-9]{40}$/.test(base || '') || !/^[a-f0-9]{40}$/.test(head || '')) throw new Error('Missing PR commit hashes');
  // Three-dot diff selects changes introduced by this branch. No rename
  // detection: both the old and new paths must affect check selection.
  return git(['diff', '--name-only', '--no-renames', '-z', `${base}...${head}`, '--']).split('\0').filter(Boolean);
}
function main() {
  const event = JSON.parse(fs.readFileSync(process.env.GITHUB_EVENT_PATH, 'utf8'));
  let checks;
  try { checks = select(process.env.GITHUB_EVENT_NAME === 'pull_request' ? changedPaths(event) : [], process.env.GITHUB_EVENT_NAME !== 'pull_request'); }
  catch (error) { console.error(`Cannot select checks; running all checks: ${error.message}`); checks = select([], true); }
  fs.appendFileSync(process.env.GITHUB_OUTPUT, `checks=${JSON.stringify(checks)}\n`);
  console.log('Selected checks:', Object.keys(checks).filter(job => checks[job]).join(', '));
}
module.exports = { jobs, select, changedPaths };
if (require.main === module) main();
