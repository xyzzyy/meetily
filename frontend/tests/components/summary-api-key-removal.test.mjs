import assert from 'node:assert/strict';
import fs from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
import ts from 'typescript';
import React from 'react';
import { act, create } from 'react-test-renderer';
import * as jsxRuntime from 'react/jsx-runtime';

const source = fs.readFileSync(new URL('../../src/components/ModelSettingsModal.tsx', import.meta.url), 'utf8');
const compiled = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020, jsx: ts.JsxEmit.ReactJSX },
});
const ui = new Proxy({}, { get: (_, name) => String(name) });
const downloads = new Set();

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

async function mount({ provider = 'openai', confirm = async () => true, remove = async () => {},
  save = async () => {}, existingKey = `${provider}-test-key`, skipInitialFetch = true,
  secondarySkipInitialFetch = skipInitialFetch, getModelConfig = async () => null,
  getApiKey = async () => '', copies = 1 } = {}) {
  const original = { provider, model: 'saved-model', whisperModel: 'large-v3', apiKey: existingKey };
  const initialKeys = { openai: null, claude: null, groq: null, openrouter: null, [provider]: original.apiKey };
  const calls = [];
  const notices = [];
  const saves = [];
  let context;
  let parentConfig;
  let renderer;
  let setShowPrimary;
  const localStorage = { getItem: () => null, setItem: () => {} };
  const require = name => {
    if (name === 'react') return React;
    if (name === 'react/jsx-runtime') return jsxRuntime;
    if (name === '@/contexts/ConfigContext') return { useConfig: () => context };
    if (name === '@/components/Sidebar/SidebarProvider' || name === './Sidebar/SidebarProvider') {
      return { useSidebar: () => ({ serverAddress: null }) };
    }
    if (name === '@/contexts/OllamaDownloadContext') {
      return { useOllamaDownload: () => ({ isDownloading: () => false, getProgress: () => 0, downloadingModels: downloads }) };
    }
    if (name === '@tauri-apps/api/core') return { invoke: async (command, args) => {
      if (command === 'api_delete_api_key') { calls.push({ command, args }); return remove(args.provider); }
      if (command === 'api_get_auto_generate_setting') return false;
      if (command === 'api_get_model_config') return getModelConfig();
      if (command === 'api_get_api_key') return getApiKey(args.provider);
      return [];
    } };
    if (name === '@tauri-apps/plugin-dialog') return { confirm: (...args) => confirm(...args) };
    if (name === 'sonner') return { toast: new Proxy({}, { get: (_, kind) => (...args) => notices.push({ kind, args }) }) };
    if (name === '@/lib/utils') return { cn: (...parts) => parts.filter(Boolean).join(' '), isOllamaNotInstalledError: () => false };
    if (name === '@/components/BuiltInModelManager') return { BuiltInModelManager: 'BuiltInModelManager' };
    if (name === 'lucide-react' || name.startsWith('@/components/ui/')) return ui;
    throw new Error(`Unexpected import: ${name}`);
  };
  const module = { exports: {} };
  vm.runInNewContext(compiled.outputText, { module, exports: module.exports, require, localStorage,
    console: { log() {}, warn() {}, error() {} }, URL, setTimeout, clearTimeout, Set, Map });
  const ModelSettingsModal = module.exports.ModelSettingsModal;
  function View() {
    const [modelConfig, setModelConfig] = React.useState(original);
    const [keys, setKeys] = React.useState(initialKeys);
    const [parent, setParent] = React.useState(original);
    const [busy, setBusy] = React.useState(false);
    const busyRef = React.useRef(false);
    const keyChanges = React.useRef({});
    const [showPrimary, updateShowPrimary] = React.useState(true);
    setShowPrimary = updateShowPrimary;
    parentConfig = parent;
    context = { modelConfig, setModelConfig, providerApiKeys: keys,
      updateProviderApiKey: (keyProvider, value) => {
        keyChanges.current[keyProvider] = { version: (keyChanges.current[keyProvider]?.version ?? 0) + 1, value };
        setKeys(previous => ({ ...previous, [keyProvider]: value }));
      },
      getProviderApiKeyChange: keyProvider => keyChanges.current[keyProvider] || { version: 0, value: null },
      isModelSettingsBusy: busy,
      runModelSettingsOperation: async operation => {
        if (busyRef.current) return false;
        busyRef.current = true;
        setBusy(true);
        try { await operation(); return true; }
        finally { busyRef.current = false; setBusy(false); }
      } };
    const modal = skip => React.createElement(ModelSettingsModal, { modelConfig: parent, setModelConfig: setParent,
      onSave: value => { saves.push(value); return save(value); }, skipInitialFetch: skip });
    if (copies === 1) return modal(skipInitialFetch);
    return React.createElement(React.Fragment, null,
      showPrimary && React.createElement('test-primary', null, modal(skipInitialFetch)),
      React.createElement('test-secondary', null, modal(secondarySkipInitialFetch)));
  }
  await act(async () => { renderer = create(React.createElement(View)); });
  const button = label => renderer.root.findAll(node => node.type === 'Button' && node.props.children === label)[0];
  const keyInput = () => renderer.root.find(node => node.type === 'Input' && node.props.placeholder === 'Enter your API key');
  return { calls, notices, saves, button, keyInput, get context() { return context; }, get parent() { return parentConfig; },
    get root() { return renderer.root; }, setShowPrimary: value => setShowPrimary(value),
    unmount: () => act(async () => renderer.unmount()) };
}

for (const provider of ['openai', 'claude', 'groq', 'openrouter']) {
  test(`removes the saved ${provider} summary key and clears the active settings`, async () => {
    let confirmation;
    const view = await mount({ provider, confirm: async (...args) => { confirmation = args; return true; } });
    try {
      assert.ok(view.button('Remove saved API key'));
      await act(async () => view.button('Remove saved API key').props.onClick());
      assert.equal(view.calls.length, 1);
      assert.equal(view.calls[0].command, 'api_delete_api_key');
      assert.equal(view.calls[0].args.provider, provider);
      assert.match(confirmation[0], /Transcription keys are separate/);
      assert.equal(view.context.providerApiKeys[provider], null);
      assert.equal(view.context.modelConfig.apiKey, null);
      assert.equal(view.parent.apiKey, null);
      assert.equal(view.keyInput().props.value, '');
      assert.equal(view.button('Remove saved API key'), undefined);
      assert.equal(view.button('Save').props.disabled, true);
      assert.equal(view.notices.at(-1).kind, 'success');
      assert.equal(view.saves.length, 0, 'removing the key does not save unrelated edits');
    } finally { await view.unmount(); }
  });
}

test('cancelling confirmation keeps the saved and edited key', async () => {
  const view = await mount({ confirm: async () => false });
  try {
    await act(async () => view.keyInput().props.onChange({ target: { value: 'unsaved-test-key' } }));
    await act(async () => view.button('Remove saved API key').props.onClick());
    assert.equal(view.calls.length, 0);
    assert.equal(view.context.providerApiKeys.openai, 'openai-test-key');
    assert.equal(view.keyInput().props.value, 'unsaved-test-key');
    assert.ok(view.button('Remove saved API key'));
  } finally { await view.unmount(); }
});

test('a deletion failure retains the saved key and allows a retry', async () => {
  let attempt = 0;
  const view = await mount({ remove: async () => { if (++attempt === 1) throw new Error('storage unavailable'); } });
  try {
    await act(async () => view.button('Remove saved API key').props.onClick());
    assert.equal(view.context.providerApiKeys.openai, 'openai-test-key');
    assert.equal(view.context.modelConfig.apiKey, 'openai-test-key');
    assert.equal(view.keyInput().props.value, 'openai-test-key');
    assert.equal(view.notices.at(-1).kind, 'error');
    await act(async () => view.button('Remove saved API key').props.onClick());
    assert.equal(attempt, 2);
    assert.equal(view.context.providerApiKeys.openai, null);
  } finally { await view.unmount(); }
});

test('a replacement key can be saved after removal', async () => {
  const view = await mount();
  try {
    await act(async () => view.button('Remove saved API key').props.onClick());
    assert.equal(view.keyInput().props.disabled, false);
    await act(async () => view.keyInput().props.onChange({ target: { value: 'replacement-test-key' } }));
    assert.equal(view.button('Save').props.disabled, false);
    await act(async () => view.button('Save').props.onClick());
    assert.equal(view.saves[0].apiKey, 'replacement-test-key');
    assert.equal(view.context.providerApiKeys.openai, 'replacement-test-key');
  } finally { await view.unmount(); }
});

test('an unsaved key does not show an action to remove a saved key', async () => {
  const view = await mount({ existingKey: null });
  try {
    assert.equal(view.button('Remove saved API key'), undefined);
    await act(async () => view.keyInput().props.onChange({ target: { value: 'unsaved-test-key' } }));
    assert.equal(view.button('Remove saved API key'), undefined);
    assert.equal(view.button('Save').props.disabled, false);
  } finally { await view.unmount(); }
});

test('removal cannot overtake a pending save of the old key', async () => {
  const pendingSave = deferred();
  const view = await mount({ save: () => pendingSave.promise });
  try {
    const remove = view.button('Remove saved API key').props.onClick;
    let saving;
    await act(async () => { saving = view.button('Save').props.onClick(); });
    assert.equal(view.button('Saving...').props.disabled, true);
    assert.equal(view.button('Remove saved API key').props.disabled, true);
    await act(async () => { await remove(); });
    assert.equal(view.calls.length, 0);
    await act(async () => { pendingSave.resolve(); await saving; });
    assert.equal(view.button('Remove saved API key').props.disabled, false);
    await act(async () => view.button('Remove saved API key').props.onClick());
    assert.equal(view.calls.length, 1);
    assert.equal(view.context.providerApiKeys.openai, null);
  } finally { await view.unmount(); }
});

test('a save stays protected when its settings view unmounts', async () => {
  const pendingSave = deferred();
  const view = await mount({ save: () => pendingSave.promise, copies: 2 });
  const findButton = (container, label) => container.find(node => node.type === 'Button' && node.props.children === label);
  try {
    const first = view.root.findByType('test-primary');
    const second = view.root.findByType('test-secondary');
    const remove = findButton(second, 'Remove saved API key').props.onClick;
    let saving;
    await act(async () => { saving = findButton(first, 'Save').props.onClick(); });
    assert.equal(findButton(second, 'Remove saved API key').props.disabled, true);
    await act(async () => view.setShowPrimary(false));
    await act(async () => { await remove(); });
    assert.equal(view.calls.length, 0);
    await act(async () => { pendingSave.resolve(); await saving; });
    assert.equal(findButton(second, 'Remove saved API key').props.disabled, false);
    await act(async () => findButton(second, 'Remove saved API key').props.onClick());
    assert.equal(view.calls.length, 1);
    assert.equal(view.context.providerApiKeys.openai, null);
  } finally { await view.unmount(); }
});

test('removal waits for the initial model configuration read', async () => {
  const initialRead = deferred();
  const view = await mount({ skipInitialFetch: false, getModelConfig: () => initialRead.promise });
  try {
    assert.equal(view.button('Remove saved API key').props.disabled, true);
    await act(async () => { initialRead.resolve(null); await initialRead.promise; });
    assert.equal(view.button('Remove saved API key').props.disabled, false);
    await act(async () => view.button('Remove saved API key').props.onClick());
    assert.equal(view.calls.length, 1);
  } finally { await view.unmount(); }
});

test('another settings view cannot restore a key from an older model configuration read', async () => {
  const oldRead = deferred();
  const view = await mount({ copies: 2, secondarySkipInitialFetch: false, getModelConfig: () => oldRead.promise });
  try {
    const first = view.root.findByType('test-primary');
    const remove = first.find(node => node.type === 'Button' && node.props.children === 'Remove saved API key');
    await act(async () => remove.props.onClick());
    assert.equal(view.context.modelConfig.apiKey, null);
    await act(async () => {
      oldRead.resolve({ provider: 'openai', model: 'saved-model', whisperModel: 'large-v3', apiKey: 'openai-test-key' });
      await oldRead.promise;
    });
    assert.equal(view.context.providerApiKeys.openai, null);
    assert.equal(view.context.modelConfig.apiKey, null);
    const inputs = view.root.findAll(node => node.type === 'Input' && node.props.placeholder === 'Enter your API key');
    assert.deepEqual(inputs.map(input => input.props.value), ['', '']);
  } finally { await view.unmount(); }
});

test('another settings view cannot restore a key from an older key lookup', async () => {
  const oldKeyRead = deferred();
  const view = await mount({ copies: 2, secondarySkipInitialFetch: false,
    getModelConfig: async () => ({ provider: 'openai', model: 'saved-model', whisperModel: 'large-v3', apiKey: null }),
    getApiKey: () => oldKeyRead.promise });
  try {
    const first = view.root.findByType('test-primary');
    const remove = first.find(node => node.type === 'Button' && node.props.children === 'Remove saved API key');
    await act(async () => remove.props.onClick());
    await act(async () => { oldKeyRead.resolve('openai-test-key'); await oldKeyRead.promise; });
    assert.equal(view.context.providerApiKeys.openai, null);
    assert.equal(view.context.modelConfig.apiKey, null);
    const inputs = view.root.findAll(node => node.type === 'Input' && node.props.placeholder === 'Enter your API key');
    assert.deepEqual(inputs.map(input => input.props.value), ['', '']);
  } finally { await view.unmount(); }
});

test('a pending deletion prevents duplicate actions and does not clear a newly selected provider', async () => {
  const removal = deferred();
  const view = await mount({ remove: () => removal.promise });
  try {
    let operation;
    const start = view.button('Remove saved API key').props.onClick;
    await act(async () => { operation = start(); await start(); });
    assert.equal(view.calls.length, 1);
    assert.equal(view.button('Removing key...').props.disabled, true);
    assert.equal(view.button('Save').props.disabled, true);
    const providerSelect = view.root.find(node => node.type === 'Select' && node.props.value === 'openai');
    assert.equal(providerSelect.props.disabled, true);
    assert.equal(view.keyInput().props.disabled, true);
    await act(async () => {
      view.context.updateProviderApiKey('claude', 'claude-test-key');
      view.context.setModelConfig(previous => ({ ...previous, provider: 'claude', apiKey: 'claude-test-key' }));
    });
    await act(async () => { removal.resolve(); await operation; });
    assert.equal(view.calls[0].args.provider, 'openai');
    assert.equal(view.context.providerApiKeys.openai, null);
    assert.equal(view.context.providerApiKeys.claude, 'claude-test-key');
    assert.equal(view.context.modelConfig.provider, 'claude');
    assert.equal(view.context.modelConfig.apiKey, 'claude-test-key');
    assert.equal(view.keyInput().props.value, 'claude-test-key');
  } finally { await view.unmount(); }
});
