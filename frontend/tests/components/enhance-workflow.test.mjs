import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import ts from 'typescript';

const source = fs.readFileSync(new URL('../../src/components/MeetingDetails/RetranscribeDialog.tsx', import.meta.url), 'utf8');
const ast = ts.createSourceFile('RetranscribeDialog.tsx', source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
const names = [];
function visit(node) {
  if (ts.isVariableDeclaration(node) && ts.isArrayBindingPattern(node.name)
    && node.initializer && ts.isCallExpression(node.initializer)
    && node.initializer.expression.getText(ast) === 'useState') names.push(node.name.elements[0].name.getText(ast));
  ts.forEachChild(node, visit);
}
visit(ast);
function dialog(provider, loading = false, selected = true, supported = true, checking = false, review = null, options = {}) {
  let cursor = 0;
  const states = new Map();
  const calls = [];
  const effects = [];
  const listeners = new Map();
  const model = { provider, name: `${provider}-selected`, displayName: provider, size_mb: 100 };
  const jsx = (type, props) => ({ type, props: props || {} });
  const hooks = { useState(initial) {
    const name = names[cursor++];
    if (!states.has(name)) states.set(name, name === 'review' ? review : initial);
    return [states.get(name), value => states.set(name, typeof value === 'function' ? value(states.get(name)) : value)];
  }, useEffect(effect) { effects.push(effect); }, useRef: value => ({ current: value }), useMemo: fn => fn() };
  const require = name => {
    if (name === 'react') return hooks;
    if (name === 'react/jsx-runtime') return { jsx, jsxs: jsx, Fragment: 'Fragment' };
    if (name === '@/contexts/ConfigContext') return { useConfig: () => ({ selectedLanguage: 'en', transcriptModelConfig: {},
      diarizationSettings: { enabled: true }, isLoadingDiarizationSettings: false }) };
    if (name === '@/hooks/useSpeakerRefinementAvailability') return { useSpeakerRefinementAvailability: () => ({ supported, checking, unavailableReason: supported ? null : 'Speaker analysis is unavailable.' }) };
    if (name === '@/hooks/useTranscriptionModels') return { useTranscriptionModels: () => ({ availableModels: [model],
      selectedModelKey: selected ? `${provider}:${model.name}` : '', loadingModels: loading,
      setSelectedModelKey() {}, fetchModels() {}, resetSelection() {} }) };
    if (name === '@/lib/transcript-comparison') {
      const helper = { exports: {} };
      const helperSource = fs.readFileSync(new URL('../../src/lib/transcript-comparison.ts', import.meta.url), 'utf8');
      const compiledHelper = ts.transpileModule(helperSource, { compilerOptions: { module: ts.ModuleKind.CommonJS } });
      vm.runInNewContext(compiledHelper.outputText, { module: helper, exports: helper.exports });
      return helper.exports;
    }
    if (name === '@/lib/funasr') {
      const helper = { exports: {} };
      const helperSource = fs.readFileSync(new URL('../../src/lib/funasr.ts', import.meta.url), 'utf8');
      const compiledHelper = ts.transpileModule(helperSource, { compilerOptions: { module: ts.ModuleKind.CommonJS } });
      vm.runInNewContext(compiledHelper.outputText, { require, module: helper, exports: helper.exports });
      return helper.exports;
    }
    if (name === '@/constants/languages') return { LANGUAGES: [] };
    if (name === '@tauri-apps/api/core') return { invoke: async (command, args) => {
      calls.push({ command, args }); return options.invoke?.(command, args);
    } };
    if (name === '@tauri-apps/api/event') return { listen: async (name, handler) => {
      listeners.set(name, handler); return () => listeners.delete(name);
    } };
    if (name === 'sonner') return { toast: { success() {}, warning() {}, info() {} } };
    return new Proxy({}, { get: (_, key) => String(key) });
  };
  const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS,
    target: ts.ScriptTarget.ES2020, jsx: ts.JsxEmit.ReactJSX } });
  const module = { exports: {} };
  vm.runInNewContext(compiled.outputText, { module, exports: module.exports, require, console });
  const render = () => {
    cursor = 0;
    return module.exports.RetranscribeDialog({ open: true, meetingId: 'meeting', meetingFolderPath: '/audio', onOpenChange() {} });
  };
  const tree = render();
  return { tree, states, calls, render, listeners, async mount() {
    for (const effect of effects.splice(0)) effect();
    await new Promise(resolve => setImmediate(resolve));
  } };
}
function nodes(node) {
  if (!node || typeof node !== 'object') return [];
  if (Array.isArray(node)) return node.flatMap(nodes);
  return [node, ...nodes(node.props?.children)];
}
const start = tree => nodes(tree).find(node => node.type === 'Button' && node.props.className === 'bg-blue-600 hover:bg-blue-700');
for (const provider of ['parakeet', 'whisper', 'funasr', 'nemotron']) {
  const instance = dialog(provider);
  assert.equal(start(instance.tree).props.disabled, false);
  await start(instance.tree).props.onClick();
  assert.equal(instance.calls.length, 1);
  assert.equal(instance.calls[0].command, 'start_retranscription_command');
  assert.equal(instance.calls[0].args.provider, provider);
  assert.equal(instance.calls[0].args.model, `${provider}-selected`);
  assert.equal(instance.calls[0].args.assignSpeakers, true);
}
for (const [loading, selected] of [[true, true], [false, false]]) {
  const instance = dialog('parakeet', loading, selected);
  assert.equal(start(instance.tree).props.disabled, true);
  await start(instance.tree).props.onClick();
  assert.equal(instance.calls.length, 0, 'a missing selection must not default to another engine');
}
// Unsupported systems can still transcribe, without starting speaker analysis.
{
  const instance = dialog('parakeet', false, true, false);
  const toggle = nodes(instance.tree).find(node => node.type === 'Switch');
  assert.equal(toggle.props.checked, false);
  assert.equal(toggle.props.disabled, true);
  assert.equal(start(instance.tree).props.disabled, false);
  await start(instance.tree).props.onClick();
  assert.equal(instance.calls[0].args.assignSpeakers, false);
}
// Wait for the capability check before applying the default speaker choice.
{
  const instance = dialog('parakeet', false, true, false, true);
  assert.equal(start(instance.tree).props.disabled, true);
  await start(instance.tree).props.onClick();
  assert.equal(instance.calls.length, 0);
}
console.log('Enhance workflow: all checks passed');

// A pending result can be applied or discarded.
for (const action of ['Keep saved transcript', 'Apply proposed transcript']) {
  const instance = dialog('whisper', false, true, true, false, {
    id: 'review-1', original: [{ transcript: 'Andrew feel free to drop or stay' }],
    proposed: [{ text: 'Enter feel free to drop' }], warnings: ['Check changed wording.'],
  });
  assert.equal(start(instance.tree), undefined, 'review must not start another model run');
  const button = nodes(instance.tree).find(node => node.type === 'Button' && node.props.children === action);
  assert.ok(button, action);
  await button.props.onClick();
  assert.equal(instance.calls[0].command, action === 'Keep saved transcript' ? 'discard_enhancement_review' : 'apply_enhancement_review');
  assert.equal(instance.calls[0].args.reviewId, 'review-1');
  assert.equal(instance.calls.length, 1);
}

// A failed automatic apply exposes the pending result without a close/reopen.
{
  let pending = null;
  const instance = dialog('whisper', false, true, true, false, null, {
    invoke: command => command === 'get_enhancement_review' ? pending : undefined,
  });
  await instance.mount();
  pending = { id: 'retry-1', applied: false, original: [{ transcript: 'Before' }],
    proposed: [{ text: 'After' }], warnings: [] };
  await instance.listeners.get('retranscription-error')({ payload: {
    meeting_id: 'another-meeting', error: 'unrelated',
  } });
  assert.equal(instance.states.get('error'), null);
  await instance.listeners.get('retranscription-error')({ payload: {
    meeting_id: 'meeting', error: 'Apply failed; the saved transcript was kept.',
  } });
  const tree = instance.render();
  assert.equal(instance.states.get('review').id, 'retry-1');
  assert.equal(instance.states.get('loadingReview'), false);
  assert.equal(start(tree), undefined);
  assert.ok(nodes(tree).some(node => node.props.children === 'Keep saved transcript'));
  const apply = nodes(tree).find(node => node.type === 'Button' && node.props.children === 'Apply proposed transcript');
  assert.ok(apply);
  await apply.props.onClick();
  assert.equal(instance.calls.at(-1).command, 'apply_enhancement_review');
  assert.equal(instance.calls.at(-1).args.reviewId, 'retry-1');
  assert.equal(instance.calls.some(call => call.command === 'start_retranscription_command'), false);
}

{
  const instance = dialog('whisper', false, true, true, false, {
    id: 'applied-1', applied: true, original: [{ transcript: 'Andrew' }],
    proposed: [{ text: 'Enter' }], warnings: [],
  });
  const undo = nodes(instance.tree).find(node => node.type === 'Button' && node.props.children === 'Undo enhancement');
  assert.ok(undo, 'an applied result needs a visible Undo action');
  assert.equal(nodes(instance.tree).some(node => node.type === 'Button' && node.props.children === 'Apply proposed transcript'), false);
  await undo.props.onClick();
  assert.equal(instance.calls[0].command, 'undo_enhancement');
  assert.equal(instance.calls[0].args.reviewId, 'applied-1');
}

// Nemotron supplies speaker turns without the separate refinement model.
{
  const instance = dialog('nemotron', false, true, false);
  const toggle = nodes(instance.tree).find(node => node.type === 'Switch');
  assert.equal(toggle.props.checked, true);
  assert.equal(toggle.props.disabled, false);
  await start(instance.tree).props.onClick();
  assert.equal(instance.calls[0].args.provider, 'nemotron');
  assert.equal(instance.calls[0].args.assignSpeakers, true);
}
