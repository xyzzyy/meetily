import assert from 'node:assert/strict';
import fs from 'node:fs';
import test from 'node:test';
import { format } from 'node:util';
import vm from 'node:vm';
import ts from 'typescript';
import React from 'react';
import { act, create } from 'react-test-renderer';

const source = fs.readFileSync(new URL('../../src/hooks/meeting-details/useModelConfiguration.ts', import.meta.url), 'utf8');
const compiled = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, esModuleInterop: true },
}).outputText;

for (const provider of ['ollama', 'custom-openai']) {
  test(`${provider} configuration logs exclude credentials during load, update, and save`, async t => {
    const config = {
      provider, model: 'model', whisperModel: 'large-v3', apiKey: 'test-provider-key',
      ollamaEndpoint: 'http://test-user:test-password@localhost:11434?token=test-ollama-token',
    };
    const customConfig = {
      model: 'custom-model', apiKey: 'test-custom-key',
      endpoint: 'https://test-user:test-password@example.com/v1?token=test-custom-token',
    };
    const listeners = new Map();
    const saved = [];
    const logs = [];
    const module = { exports: {} };
    vm.runInNewContext(compiled, {
      module, exports: module.exports,
      require: name => {
        if (name === 'react') return React;
        if (name === '@tauri-apps/api/core') return { invoke: async (command, payload) => {
          if (command === 'api_get_model_config') return { ...config };
          if (command === 'api_get_custom_openai_config') return { ...customConfig };
          if (command === 'api_save_model_config') { saved.push(payload); return; }
          throw new Error(`Unexpected command: ${command}`);
        } };
        if (name === '@tauri-apps/api/event') return {
          listen: async (event, callback) => {
            listeners.set(event, callback);
            return () => listeners.delete(event);
          },
          emit: async (event, payload) => listeners.get(event)({ payload }),
        };
        if (name === '@/lib/analytics') return {
          trackModelChanged: async () => {}, trackSettingsChanged: async () => {},
        };
        if (name === 'sonner') return { toast: {
          success() {}, error() { assert.fail('Configuration save failed'); },
        } };
        throw new Error(`Unexpected import: ${name}`);
      },
      console: { log: (...args) => logs.push(format(...args)), warn() {}, error: assert.fail },
    });

    let hook;
    let renderer;
    function View() {
      hook = module.exports.useModelConfiguration({ serverAddress: null });
      return null;
    }
    await act(async () => { renderer = create(React.createElement(View)); });
    t.after(() => act(async () => renderer.unmount()));
    assert.equal(hook.modelConfig.apiKey, config.apiKey);
    assert.equal(hook.modelConfig.ollamaEndpoint, config.ollamaEndpoint);
    if (provider === 'custom-openai') {
      assert.equal(hook.modelConfig.customOpenAIApiKey, customConfig.apiKey);
      assert.equal(hook.modelConfig.customOpenAIEndpoint, customConfig.endpoint);
    }

    const updated = { ...hook.modelConfig, model: 'updated-model' };
    await act(async () => listeners.get('model-config-updated')({ payload: updated }));
    await act(async () => hook.handleSaveModelConfig());
    assert.equal(saved.length, 1);
    assert.equal(saved[0].model, updated.model);
    assert.equal(saved[0].apiKey, config.apiKey);
    assert.equal(saved[0].ollamaEndpoint, config.ollamaEndpoint);
    for (const secret of [config.apiKey, customConfig.apiKey, 'test-user', 'test-password', 'test-ollama-token', 'test-custom-token']) {
      assert.ok(!logs.join('\n').includes(secret), `Console logs contain ${secret}`);
    }
  });
}
