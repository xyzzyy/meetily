#!/usr/bin/env node

const { execFileSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

if (process.platform !== 'darwin') {
  process.exit(0);
}
if (os.arch() !== 'arm64') {
  console.error('Speaker recognition currently requires an Apple Silicon Mac.');
  process.exit(1);
}

const frontend = path.resolve(__dirname, '..');
const packageDirectory = path.resolve(frontend, '..', 'diarization-sidecar');
const output = path.join(
  frontend,
  'src-tauri',
  'binaries',
  'meetily-diarization-aarch64-apple-darwin'
);
// Ask xcrun last: it finds the SDK in a full Xcode install too.
function xcrunSdkPath() {
  try {
    return execFileSync('xcrun', ['--sdk', 'macosx', '--show-sdk-path'], { encoding: 'utf8' }).trim();
  } catch {
    return undefined;
  }
}
const sdkCandidates = [
  process.env.SDKROOT,
  '/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk',
];
const sdk = sdkCandidates.filter(Boolean).find(candidate => fs.existsSync(candidate))
  ?? [xcrunSdkPath()].filter(Boolean).find(candidate => fs.existsSync(candidate));

if (!sdk) {
  console.error('A macOS SDK (from Xcode or Apple Command Line Tools) is required to build speaker recognition.');
  process.exit(1);
}

const cxxIncludes = path.join(sdk, 'usr', 'include', 'c++', 'v1');
const environment = {
  ...process.env,
  CLANG_MODULE_CACHE_PATH: '/tmp/meetily-clang-cache',
  SWIFT_MODULECACHE_PATH: '/tmp/meetily-swift-cache',
  CPLUS_INCLUDE_PATH: cxxIncludes,
  MACOSX_DEPLOYMENT_TARGET: '14.4',
};

console.log('Building the local speaker recognition helper…');
execFileSync('swift', [
  'build',
  '-c', 'release',
  '--product', 'meetily-diarization',
  '-Xcc', '-isystem',
  '-Xcc', cxxIncludes,
  '-Xswiftc', '-strict-concurrency=minimal',
  '-Xswiftc', '-swift-version',
  '-Xswiftc', '5',
  '-Xswiftc', '-enable-bare-slash-regex',
], {
  cwd: packageDirectory,
  env: environment,
  stdio: 'inherit',
});

const builtBinary = path.join(packageDirectory, '.build', 'release', 'meetily-diarization');
fs.mkdirSync(path.dirname(output), { recursive: true });
fs.copyFileSync(builtBinary, output);
fs.chmodSync(output, 0o755);
execFileSync('codesign', ['--force', '--sign', '-', output], { stdio: 'inherit' });
