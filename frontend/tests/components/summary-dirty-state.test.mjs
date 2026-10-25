import assert from 'node:assert/strict';
import fs from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
import ts from 'typescript';
import React from 'react';
import { act, create } from 'react-test-renderer';

function loadSummaryView() {
  const source = fs.readFileSync(new URL('../../src/components/AISummary/BlockNoteSummaryView.tsx', import.meta.url), 'utf8');
  const compiled = ts.transpileModule(source, { compilerOptions: {
    module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.React, esModuleInterop: true,
  } });
  const require = name => {
    if (name === 'react') return React;
    if (name === 'next/dynamic') return () => () => null;
    if (name === '@blocknote/react') return { useCreateBlockNote: () => null };
    if (name === '@blocknote/shadcn') return { BlockNoteView: () => null };
    if (name === '@blocknote/shadcn/style.css') return {};
    if (name === '@/lib/blocknote-markdown') return {};
    if (name === '@/contexts/ThemeContext') return { useTheme: () => ({ resolvedTheme: 'light' }) };
    if (name === './index') return { AISummary: ({ onSummaryChange }) => React.createElement('button', {
      onClick: () => onSummaryChange({ Agenda: { title: 'Agenda', blocks: [{ id: 'a', content: 'Unsaved edit' }] } }),
    }, 'Edit') };
    throw new Error(`Unexpected import: ${name}`);
  };
  const module = { exports: {} };
  vm.runInNewContext(compiled.outputText, {
    module, exports: module.exports, require, React,
    console: { log() {}, warn() {}, error() {} }, setTimeout: callback => callback(),
  });
  return module.exports.BlockNoteSummaryView;
}

test('legacy edits stay dirty when the editor returns after a stopped or failed generation', async t => {
  const View = loadSummaryView();
  let dirty;
  function Parent({ visible }) {
    const [summary, setSummary] = React.useState({ Agenda: { title: 'Agenda', blocks: [] } });
    const [isDirty, setDirty] = React.useState(false);
    dirty = isDirty;
    return visible ? React.createElement(View, {
      summaryData: summary, onDirtyChange: setDirty,
      onSummaryChange: value => { setSummary(value); setDirty(true); },
    }) : null;
  }
  let renderer;
  await act(async () => { renderer = create(React.createElement(Parent, { visible: true })); });
  t.after(() => renderer.unmount());
  await act(async () => { renderer.root.findByType('button').props.onClick(); });
  assert.equal(dirty, true);
  await act(async () => { renderer.update(React.createElement(Parent, { visible: false })); });
  await act(async () => { renderer.update(React.createElement(Parent, { visible: true })); });
  assert.equal(dirty, true);
});

test('a newly mounted BlockNote editor still reports its own clean state', async t => {
  const View = loadSummaryView();
  const changes = [];
  let renderer;
  await act(async () => { renderer = create(React.createElement(View, {
    summaryData: { summary_json: [] }, onDirtyChange: value => changes.push(value),
  })); });
  t.after(() => renderer.unmount());
  assert.deepEqual(changes, [false]);
});
