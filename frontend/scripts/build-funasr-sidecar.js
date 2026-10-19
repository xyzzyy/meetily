#!/usr/bin/env node

const { copyFileSync, existsSync, mkdirSync } = require('fs');
const { execFileSync } = require('child_process');
const path = require('path');

const frontendDir = path.resolve(__dirname, '..');
const repositoryRoot = path.resolve(frontendDir, '..');
const sourceDir = path.join(repositoryRoot, 'funasr-helper');

function detectTarget() {
  if (process.env.TAURI_ENV_TARGET_TRIPLE) {
    return process.env.TAURI_ENV_TARGET_TRIPLE;
  }
  if (process.env.CARGO_BUILD_TARGET) {
    return process.env.CARGO_BUILD_TARGET;
  }
  const version = execFileSync('rustc', ['-vV'], { encoding: 'utf8' });
  const host = version.match(/^host:\s*(.+)$/m);
  if (!host) throw new Error('Unable to determine the Rust host target');
  return host[1].trim();
}

const target = detectTarget();
const extension = target.includes('windows') ? '.exe' : '';
const destinationDir = path.join(frontendDir, 'src-tauri', 'binaries');
const destination = path.join(
  destinationDir,
  `funasr-helper-${target}${extension}`
);

if (existsSync(destination) && process.env.MEETILY_FORCE_FUNASR_BUILD !== '1') {
  console.log(`FUN-ASR sidecar already exists: ${destination}`);
  process.exit(0);
}

const buildDir = path.join(sourceDir, `build-${target}`);
mkdirSync(destinationDir, { recursive: true });

execFileSync(
  'cmake',
  ['-S', sourceDir, '-B', buildDir, '-DCMAKE_BUILD_TYPE=Release'],
  { stdio: 'inherit' }
);
execFileSync(
  'cmake',
  ['--build', buildDir, '--config', 'Release', '--target', 'funasr-helper'],
  { stdio: 'inherit' }
);

const candidates = [
  path.join(buildDir, 'bin', `funasr-helper${extension}`),
  path.join(buildDir, 'bin', 'Release', `funasr-helper${extension}`),
  path.join(buildDir, 'Release', `funasr-helper${extension}`),
];
const binary = candidates.find(existsSync);
if (!binary) {
  throw new Error(`FUN-ASR helper build succeeded but no binary was found in ${buildDir}`);
}
copyFileSync(binary, destination);
console.log(`Copied FUN-ASR sidecar to ${destination}`);
