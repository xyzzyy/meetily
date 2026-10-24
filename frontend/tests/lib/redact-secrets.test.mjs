import assert from 'node:assert/strict';
import fs from 'node:fs';
import test from 'node:test';
import ts from 'typescript';

const source = fs.readFileSync(new URL('../../src/lib/redact-secrets.ts', import.meta.url), 'utf8');
const module = { exports: {} };
// Run in this realm so deepEqual sees ordinary Objects.
new Function('module', 'exports',
  ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS } }).outputText,
)(module, module.exports);
const { redactSecrets } = module.exports;

test('masks credential fields but keeps ordinary settings readable', () => {
  const config = {
    provider: 'openai', model: 'gpt-4o', apiKey: 'sk-secret', customOpenAIApiKey: 'sk-other',
    maxTokens: 2048, ollamaEndpoint: 'http://localhost:11434',
  };
  assert.deepEqual(redactSecrets(config), {
    provider: 'openai', model: 'gpt-4o', apiKey: '***', customOpenAIApiKey: '***',
    maxTokens: 2048, ollamaEndpoint: 'http://localhost:11434',
  });
});

test('leaves empty keys alone, reaches nested values and never mutates the input', () => {
  const config = { apiKey: null, nested: { token: 'keep', api_key: 'abc' }, list: [{ apiKey: 'x' }] };
  const redacted = redactSecrets(config);
  assert.deepEqual(redacted, { apiKey: null, nested: { token: 'keep', api_key: '***' }, list: [{ apiKey: '***' }] });
  assert.equal(config.list[0].apiKey, 'x');
});

test('passes through primitives', () => {
  assert.equal(redactSecrets('text'), 'text');
  assert.equal(redactSecrets(null), null);
});
