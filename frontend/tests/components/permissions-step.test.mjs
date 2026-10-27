import assert from 'node:assert/strict';
import fs from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
import ts from 'typescript';
import React from 'react';
import { act, create } from 'react-test-renderer';

// Objects built inside the VM have a different prototype, so compare plain copies.
const plain = value => JSON.parse(JSON.stringify(value));

/** Mount the real PermissionsStep with both permissions denied and record what it invokes. */
async function mount({ rejectInvoke = (command, args) => command === 'open_system_settings' && !args } = {}) {
  const source = fs.readFileSync(new URL('../../src/components/onboarding/steps/PermissionsStep.tsx', import.meta.url), 'utf8');
  const compiled = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.React, esModuleInterop: true },
  }).outputText;
  const calls = [];
  const alerts = [];
  const rows = new Map();
  const passthrough = ({ children }) => children ?? null;
  const module = { exports: {} };
  vm.runInNewContext(compiled, {
    module, exports: module.exports, React, console: { log() {}, error() {}, warn() {} },
    alert: message => alerts.push(message),
    window: { location: { reload() {} } },
    require: name => {
      if (name === 'react') return React;
      // Rejects the way Tauri does when a required argument is missing.
      if (name === '@tauri-apps/api/core') {
        return { invoke: async (command, args) => {
          calls.push({ command, args });
          if (rejectInvoke(command, args)) throw new Error(`missing required key preferencePane`);
          return undefined;
        } };
      }
      if (name === 'lucide-react') return new Proxy({}, { get: () => () => null });
      if (name === '@/components/ui/button') return { Button: passthrough };
      if (name === '../OnboardingContainer') return { OnboardingContainer: passthrough };
      if (name === '../shared') {
        return { PermissionRow: props => { rows.set(props.title, props); return null; } };
      }
      if (name === '@/contexts/OnboardingContext') {
        return { useOnboarding: () => ({
          setPermissionStatus() {}, setPermissionsSkipped() {}, completeOnboarding: async () => {},
          permissions: { microphone: 'denied', systemAudio: 'denied' },
        }) };
      }
      return {};
    },
  });
  let renderer;
  await act(async () => { renderer = create(React.createElement(module.exports.PermissionsStep)); });
  return { calls, alerts, rows, unmount: () => act(async () => renderer.unmount()) };
}

test('a denied microphone opens the Microphone privacy pane', async () => {
  const app = await mount();
  await act(async () => app.rows.get('Microphone').onAction());
  assert.deepEqual(plain(app.calls.at(-1)), { command: 'open_system_settings', args: { preferencePane: 'Privacy_Microphone' } });
  assert.deepEqual(plain(app.alerts), [], 'no manual-instructions alert when the pane opens');
  await app.unmount();
});

test('denied system audio opens the Screen Recording privacy pane', async () => {
  const app = await mount();
  await act(async () => app.rows.get('System Audio').onAction());
  assert.deepEqual(plain(app.calls.at(-1)), { command: 'open_system_settings', args: { preferencePane: 'Privacy_ScreenCapture' } });
  assert.deepEqual(plain(app.alerts), []);
  await app.unmount();
});
