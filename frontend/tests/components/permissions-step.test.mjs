import assert from 'node:assert/strict';
import fs from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
import ts from 'typescript';
import React from 'react';
import { act, create } from 'react-test-renderer';

const compilerOptions = {
  module: ts.ModuleKind.CommonJS,
  jsx: ts.JsxEmit.React,
  esModuleInterop: true,
};

function compile(relativePath) {
  const source = fs.readFileSync(new URL(relativePath, import.meta.url), 'utf8');
  return ts.transpileModule(source, { compilerOptions }).outputText;
}

const rowCode = compile('../../src/components/onboarding/shared/PermissionRow.tsx');
const stepCode = compile('../../src/components/onboarding/steps/PermissionsStep.tsx');

async function mount() {
  const calls = [];
  const grants = new Map();
  let completed = false;
  let reloaded = false;
  const waitFor = (command, args) => {
    calls.push({ command, pane: args?.preferencePane });
    if (command === 'open_system_settings') {
      assert.ok(args?.preferencePane, 'System Settings requires a pane');
      return;
    }
    return grants.get(command) ?? false;
  };

  function useOnboarding() {
    const [permissions, setPermissions] = React.useState({
      microphone: 'not_determined',
      systemAudio: 'not_determined',
    });
    return {
      permissions,
      setPermissionStatus: (key, status) => setPermissions(previous => ({ ...previous, [key]: status })),
      setPermissionsSkipped() {},
      async completeOnboarding() { completed = true; },
    };
  }

  const Button = ({ variant, size, ...props }) => React.createElement('button', props);
  const dependencies = {
    react: React,
    'lucide-react': new Proxy({}, { get: () => () => null }),
    '@tauri-apps/api/core': { invoke: async (...args) => waitFor(...args) },
    '@/components/ui/button': { Button },
    '@/contexts/OnboardingContext': { useOnboarding },
    '@/lib/utils': { cn: (...classes) => classes.filter(Boolean).join(' ') },
    '../OnboardingContainer': { OnboardingContainer: ({ children }) => children },
  };

  function load(code) {
    const module = { exports: {} };
    vm.runInNewContext(code, {
      module,
      exports: module.exports,
      require: name => {
        assert.ok(name in dependencies, `Unexpected import: ${name}`);
        return dependencies[name];
      },
      console,
      window: { location: { reload() { reloaded = true; } } },
      alert: message => assert.fail(message),
    });
    return module.exports;
  }

  const { PermissionRow } = load(rowCode);
  dependencies['../shared'] = { PermissionRow };
  const { PermissionsStep } = load(stepCode);
  let renderer;
  await act(async () => { renderer = create(React.createElement(PermissionsStep)); });

  const row = title => renderer.root.findAllByType(PermissionRow).find(node => node.props.title === title);
  const button = (label, within = renderer.root) =>
    within.findAllByType('button').find(node => node.children.includes(label));

  return {
    calls,
    grants,
    row,
    button,
    click: node => act(async () => node.props.onClick()),
    finished: () => completed && reloaded,
    unmount: () => act(async () => renderer.unmount()),
  };
}

test('onboarding recovers from a denied mic and an unverified system audio check', async t => {
  const app = await mount();
  t.after(app.unmount);

  // Start from the same state as a new setup. Neither check has verified access yet.
  assert.equal(app.button('Finish Setup').props.disabled, true);
  for (const title of ['Microphone', 'System Audio']) {
    const enable = app.button('Enable', app.row(title));
    assert.equal(enable.props['aria-label'], `Enable ${title}`);
    await app.click(enable);
    assert.equal(app.button('Open Settings', app.row(title)).props['aria-label'], `Open Settings for ${title}`);
    assert.equal(app.button('Check Again', app.row(title)).props['aria-label'], `Check Again for ${title}`);
  }
  assert.equal(app.row('Microphone').props.status, 'denied');
  assert.equal(app.row('System Audio').props.status, 'unverified');
  assert.equal(app.button('Finish Setup').props.disabled, true);

  for (const [title, pane, command] of [
    ['Microphone', 'Privacy_Microphone', 'trigger_microphone_permission'],
    ['System Audio', 'Privacy_ScreenCapture', 'trigger_system_audio_permission_command'],
  ]) {
    await app.click(app.button('Open Settings', app.row(title)));
    assert.deepEqual(app.calls.at(-1), { command: 'open_system_settings', pane });
    app.grants.set(command, true);
    await app.click(app.button('Check Again', app.row(title)));
    assert.equal(app.row(title).props.status, 'authorized');
    assert.equal(app.button('Open Settings', app.row(title)), undefined);
  }

  assert.equal(app.button('Finish Setup').props.disabled, false);
  await app.click(app.button('Finish Setup'));
  assert.equal(app.finished(), true);
});

test('denied and unverified rows read differently and only the checked row shows progress', async t => {
  const app = await mount();
  t.after(app.unmount);

  const text = (title, value) =>
    app.row(title).findAll(node => typeof node.type === 'string' && node.children.includes(value)).length > 0;

  for (const title of ['Microphone', 'System Audio']) {
    await app.click(app.button('Enable', app.row(title)));
  }
  assert.ok(text('Microphone', 'Access denied'));
  assert.ok(!text('Microphone', 'Not verified'));
  assert.ok(text('System Audio', 'Not verified'));
  assert.ok(!text('System Audio', 'Access denied'));
  assert.match(app.row('System Audio').props.description, /Play audio on this Mac and check again/);

  let finishCheck;
  app.grants.set('trigger_system_audio_permission_command', new Promise(resolve => { finishCheck = resolve; }));
  let pendingClick;
  act(() => { pendingClick = app.button('Check Again', app.row('System Audio')).props.onClick(); });

  assert.ok(app.button('Checking...', app.row('System Audio')));
  const audioStatus = () => app.row('System Audio').findByProps({ role: 'status' });
  assert.equal(audioStatus().findByProps({ className: 'sr-only', children: 'Checking System Audio access' }).type, 'span');
  assert.equal(app.button('Checking...', app.row('Microphone')), undefined);
  assert.ok(app.button('Check Again', app.row('Microphone')));
  for (const title of ['Microphone', 'System Audio']) {
    for (const node of app.row(title).findAllByType('button')) {
      assert.equal(node.props.disabled, true, `${title} actions wait for the running check`);
    }
  }

  await act(async () => { finishCheck(true); await pendingClick; });
  assert.ok(text('System Audio', 'Access granted'));
  assert.ok(audioStatus().findAllByType('span').some(node => node.children.includes('Access granted')));
  assert.equal(app.row('System Audio').findAllByType('button').length, 0);
  assert.equal(app.button('Check Again', app.row('Microphone')).props.disabled, false);
});
