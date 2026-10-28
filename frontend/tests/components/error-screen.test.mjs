import assert from 'node:assert/strict';
import fs from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
import ts from 'typescript';
import React from 'react';
import { act, create } from 'react-test-renderer';

function load(file, { assigned = [], browser = {} } = {}) {
  const source = fs.readFileSync(new URL(`../../src/${file}`, import.meta.url), 'utf8');
  const compiled = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.React, esModuleInterop: true },
  }).outputText;
  const module = { exports: {} };
  const logged = [];
  vm.runInNewContext(compiled, {
    module, exports: module.exports, React,
    console: { error: (...args) => logged.push(args), log() {}, warn() {} },
    window: { location: { assign: url => assigned.push(url) } },
    require: name => {
      if (name === 'react') return React;
      if (name === '@/components/ErrorScreen') return { ErrorScreen: load('components/ErrorScreen.tsx', { assigned }).module.exports.ErrorScreen };
      if (name === '@/lib/theme') return load('lib/theme.ts').module.exports;
      return {};
    },
    ...browser,
  });
  return { module, logged };
}

const textOf = node => (typeof node === 'string' ? node : (node.children || []).map(textOf).join(' '));

test('the error screen explains the failure and offers a way out', async () => {
  const assigned = [];
  const { module, logged } = load('components/ErrorScreen.tsx', { assigned });
  const resets = [];
  const error = new Error('Cannot read properties of undefined');
  let renderer;
  await act(async () => {
    renderer = create(React.createElement(module.exports.ErrorScreen, { error, reset: () => resets.push('reset') }));
  });
  const tree = renderer.toJSON();
  const text = textOf(tree);
  assert.match(text, /Something went wrong/);
  assert.match(text, /Cannot read properties of undefined/);
  assert.equal(tree.props.role, 'alert');
  assert.equal(logged.length, 1, 'the error is logged for diagnosis');

  const buttons = renderer.root.findAllByType('button');
  await act(async () => buttons.find(button => button.props.children === 'Try again').props.onClick());
  assert.deepEqual([...resets], ['reset']);
  await act(async () => buttons.find(button => button.props.children === 'Go to home').props.onClick());
  assert.deepEqual([...assigned], ['/']);
  await act(async () => renderer.unmount());
});

test('the route-level boundary renders the shared screen', async () => {
  const { module } = load('app/error.tsx');
  let renderer;
  await act(async () => {
    renderer = create(React.createElement(module.exports.default, { error: new Error('boom'), reset() {} }));
  });
  assert.match(textOf(renderer.toJSON()), /Something went wrong/);
  await act(async () => renderer.unmount());
});

for (const [saved, systemDark, blocked, expected] of [
  ['dark', false, false, 'dark'],
  ['light', true, false, 'light'],
  ['system', true, false, 'dark'],
  [null, false, false, 'light'],
  ['invalid', true, false, 'dark'],
  [null, true, true, 'dark'],
]) {
  test(`the global boundary restores the theme (${saved}, system dark: ${systemDark}, storage blocked: ${blocked})`, async () => {
    let dark;
    const style = {};
    const { module } = load('app/global-error.tsx', { browser: {
      window: {
        localStorage: { getItem(key) {
          assert.equal(key, 'meetily-theme');
          if (blocked) throw new Error('Storage blocked');
          return saved;
        } },
        matchMedia(query) {
          assert.equal(query, '(prefers-color-scheme: dark)');
          return { matches: systemDark };
        },
      },
      document: { documentElement: { style, classList: { toggle(name, enabled) {
        assert.equal(name, 'dark');
        dark = enabled;
      } } } },
    } });
    let renderer;
    await act(async () => {
      renderer = create(React.createElement(module.exports.default, { error: new Error('boom'), reset() {} }));
    });
    assert.equal(renderer.toJSON().type, 'html');
    assert.match(textOf(renderer.toJSON()), /boom/);
    assert.equal(dark, expected === 'dark');
    assert.equal(style.colorScheme, expected);
    await act(async () => renderer.unmount());
  });
}
