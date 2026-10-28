import assert from 'node:assert/strict';
import fs from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
import ts from 'typescript';
import React from 'react';
import { act, create } from 'react-test-renderer';

async function mount(config, { runEffects = false } = {}) {
  const source = fs.readFileSync(new URL('../../src/components/ModelSettingsModal.tsx', import.meta.url), 'utf8');
  const compiled = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.React, esModuleInterop: true },
  }).outputText;
  const calls = [];
  const saved = [];
  const writes = [];
  const changes = [];
  const storage = new Map();
  const passthrough = ({ children }) => children ?? null;
  const module = { exports: {} };
  vm.runInNewContext(compiled, {
    module, exports: module.exports, React, URL, Map, Set, setTimeout, clearTimeout,
    console: { log() {}, error() {} },
    localStorage: {
      getItem: key => storage.get(key) ?? null,
      setItem: (key, value) => { writes.push({ key, value }); storage.set(key, value); },
    },
    require: name => {
      // Save tests omit startup requests. Loading tests run the real effects.
      if (name === 'react') return runEffects ? React : { ...React, useEffect() {} };
      if (name === '@tauri-apps/api/core') return { invoke: async (command, args) => {
        calls.push({ command, args });
        if (command === 'get_openrouter_models') return [{ id: 'test/model', name: 'Test model' }];
        if (command === 'get_groq_models') return [];
        if (command === 'api_get_auto_generate_setting') return true;
      } };
      if (name === './Sidebar/SidebarProvider') return { useSidebar: () => ({ serverAddress: '' }) };
      if (name === '@/contexts/ConfigContext') return { useConfig: () => null };
      if (name === '@/contexts/OllamaDownloadContext') return { useOllamaDownload: () => ({
        isDownloading: () => false, getProgress: () => 0, downloadingModels: new Set(),
      }) };
      if (name === '@/lib/utils') return { cn: (...values) => values.filter(Boolean).join(' ') };
      if (name === 'sonner') return { toast: { error() {} } };
      if (name === 'lucide-react') return new Proxy({}, { get: () => () => null });
      if (name === '@/components/ui/button') return { Button: props => React.createElement('button', props) };
      if (name === '@/components/ui/input') return { Input: props => React.createElement('input', props) };
      return new Proxy({}, { get: () => passthrough });
    },
  });
  let initial = { provider: 'builtin-ai', model: '', whisperModel: 'small', ...config };
  let renderer;
  const render = () => React.createElement(module.exports.ModelSettingsModal, {
    modelConfig: initial, setModelConfig: value => changes.push(value),
    onSave: value => saved.push(JSON.parse(JSON.stringify(value))), skipInitialFetch: true,
  });
  await act(async () => {
    renderer = create(render());
  });
  return {
    renderer, calls, saved, writes, changes,
    update: config => act(async () => {
      initial = { ...initial, ...config };
      renderer.update(render());
    }),
    save: () => renderer.root.findAllByType('button').find(button => button.props.children === 'Save'),
    unmount: () => act(async () => renderer.unmount()),
  };
}

for (const provider of ['openrouter', 'groq']) {
  test(`OpenRouter models load when ${provider === 'openrouter' ? 'settings open' : 'the provider changes'}`, async t => {
    const app = await mount({ provider, apiKey: 'test-key' }, { runEffects: true });
    t.after(app.unmount);
    const requests = () => app.calls.filter(call => call.command === 'get_openrouter_models');
    assert.equal(requests().length, provider === 'openrouter' ? 1 : 0);
    await app.update({ provider: 'openrouter' });
    assert.equal(requests().length, 1);
    assert.equal(app.renderer.root.findAll(node =>
      node.props.value === 'test/model' && typeof node.props.onSelect === 'function'
    ).length, 1, 'the fetched model is available for selection');
    await app.update({ provider: 'groq' });
    await app.update({ provider: 'openrouter' });
    assert.equal(requests().length, 1, 'the loaded list is reused');
  });
}

for (const provider of ['builtin-ai', 'ollama', 'openai', 'claude', 'groq', 'openrouter']) {
  test(`${provider}: an empty or blank model cannot be saved`, async () => {
    for (const model of ['', ' \t ']) {
      const app = await mount({ provider, model, apiKey: 'test-key' });
      assert.equal(app.save().props.disabled, true);
      // Calling the handler directly must also leave all saved settings untouched.
      await act(async () => app.save().props.onClick());
      assert.deepEqual(app.saved, []);
      assert.deepEqual(app.changes, []);
      assert.deepEqual(app.writes, []);
      assert.deepEqual(app.calls, []);
      await app.unmount();
    }
  });
}

test('a selected built-in model enables Save and persists the trimmed name', async () => {
  const app = await mount({ model: ' gemma3:1b ' });
  assert.equal(app.save().props.disabled, false);
  await act(async () => app.save().props.onClick());
  assert.equal(app.saved.length, 1);
  assert.equal(app.saved[0].model, 'gemma3:1b');
  assert.deepEqual(JSON.parse(app.writes[0].value), { 'builtin-ai': 'gemma3:1b' });
  await app.unmount();
});

test('custom OpenAI validates the model input, including edits after opening', async () => {
  const app = await mount({
    provider: 'custom-openai', model: '', customOpenAIModel: 'local-model',
    customOpenAIEndpoint: 'http://localhost:8080/v1',
  });
  assert.equal(app.save().props.disabled, false, 'the custom model input is the effective model');
  const input = app.renderer.root.findAllByType('input').find(node => node.props.id === 'custom-model');
  assert.ok(input);
  await act(async () => input.props.onChange({ target: { value: '  ' } }));
  assert.equal(app.save().props.disabled, true);
  await act(async () => app.save().props.onClick());
  assert.deepEqual(app.calls, []);
  assert.deepEqual(app.saved, []);
  await act(async () => input.props.onChange({ target: { value: ' local-model ' } }));
  assert.equal(app.save().props.disabled, false);
  await act(async () => app.save().props.onClick());
  assert.equal(app.calls[0].command, 'api_save_custom_openai_config');
  assert.equal(app.calls[0].args.model, 'local-model');
  assert.equal(app.saved[0].model, 'local-model');
  await app.unmount();
});

test('a model name does not bypass the existing API key check', async () => {
  const app = await mount({ provider: 'openai', model: 'gpt-4o', apiKey: ' ' });
  assert.equal(app.save().props.disabled, true);
  await act(async () => app.save().props.onClick());
  assert.deepEqual(app.saved, []);
  await app.unmount();
});
