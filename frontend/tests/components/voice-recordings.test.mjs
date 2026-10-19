// Run with: bun test frontend/tests/components/voice-recordings.test.mjs
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const ts = require(process.env.TYPESCRIPT_PATH || 'typescript');
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const sourcePath = path.join(root, 'src/components/SpeakerDiarizationSettings.tsx');
const source = fs.readFileSync(sourcePath, 'utf8');
const ast = ts.createSourceFile(sourcePath, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
const stateNames = [];
(function visit(node) {
  if (ts.isVariableDeclaration(node) && ts.isArrayBindingPattern(node.name)
      && node.initializer && ts.isCallExpression(node.initializer)
      && node.initializer.expression.getText(ast) === 'useState') {
    stateNames.push(node.name.elements[0].name.getText(ast));
  }
  ts.forEachChild(node, visit);
})(ast);
const compiled = ts.transpileModule(source, { compilerOptions: {
  module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2020, jsx: ts.JsxEmit.ReactJSX,
} });

const madior = { id: 'p1', name: 'Scrum Master Madior', modelVersion: 'v1', sampleCount: 27, speechSeconds: 60 };
const sample = (meetingId, extra = {}) => ({
  meetingId, speakerId: 'S1', meetingTitle: `Meeting ${meetingId}`, meetingCreatedAt: '2026-10-14T13:21:05Z',
  speakerLabel: 'Scrum Master Madior', speechSeconds: 30, removable: true, ...extra,
});

function makeSettings({ samples, confirmAnswer = true, removeOutcome = 'recalculated' }) {
  let cursor = 0;
  const states = new Map();
  const calls = [];
  const dialogs = [];
  let currentSamples = samples;
  const jsx = (type, props) => ({ type, props: props || {} });
  const mockRequire = name => {
    if (name === 'react') return {
      useState(initial) {
        const key = stateNames[cursor++];
        if (!states.has(key)) states.set(key, initial);
        return [states.get(key), value => states.set(key, typeof value === 'function' ? value(states.get(key)) : value)];
      },
      useEffect: () => {},
    };
    if (name === 'react/jsx-runtime') return { jsx, jsxs: jsx, Fragment: 'Fragment' };
    if (name === '@tauri-apps/api/core') return { invoke: async (command, args) => {
      calls.push({ command, args });
      if (command === 'list_speaker_profiles') return [madior];
      if (command === 'list_speaker_profile_samples') return currentSamples;
      if (command === 'remove_speaker_profile_sample') {
        currentSamples = currentSamples.filter(item => item.meetingId !== args.request.meetingId);
        return removeOutcome;
      }
      return undefined;
    } };
    if (name === '@tauri-apps/api/event') return { listen: async () => () => {} };
    if (name === '@tauri-apps/plugin-dialog') return {
      confirm: async (...args) => { dialogs.push({ kind: 'confirm', args }); return confirmAnswer; },
      message: async (...args) => { dialogs.push({ kind: 'message', args }); },
    };
    if (name === '@/contexts/ConfigContext') return { useConfig: () => ({
      diarizationSettings: { enabled: true, mode: 'live_plus_post_call', showProvisionalLabels: true },
      isLoadingDiarizationSettings: false, updateDiarizationSettings: async () => {},
    }) };
    return new Proxy({}, { get: (_, key) => String(key) });
  };
  const module = { exports: {} };
  vm.runInNewContext(compiled.outputText, { exports: module.exports, module, require: mockRequire, console, Number, Date });
  states.set('profiles', [madior]);
  const render = () => { cursor = 0; return module.exports.SpeakerDiarizationSettings(); };
  return { render, states, calls, dialogs };
}

function nodes(node) {
  if (!node || typeof node !== 'object') return [];
  if (Array.isArray(node)) return node.flatMap(nodes);
  return [node, ...nodes(node.props?.children), ...nodes(node.props?.control)];
}
function text(node) {
  if (Array.isArray(node)) return node.map(text).join('');
  if (node === null || node === undefined || typeof node === 'boolean') return '';
  if (typeof node !== 'object') return String(node);
  return text(node.props?.children);
}
const buttons = (tree, label) => nodes(tree).filter(node => node.type === 'Button' && text(node) === label);
async function settle() { for (let i = 0; i < 20; i++) await Promise.resolve(); }

// Opening a voice lists the meetings that make it up.
{
  const settings = makeSettings({ samples: [sample('m1'), sample('m2')] });
  buttons(settings.render(), 'Recordings')[0].props.onClick();
  await settle();
  const tree = settings.render();
  assert.ok(text(tree).includes('Meeting m1'));
  assert.ok(text(tree).includes('Meeting m2'));
  assert.equal(buttons(tree, 'Remove').length, 2);
}
// Removing one of several rebuilds the voice without asking, and refreshes the list.
{
  const settings = makeSettings({ samples: [sample('m1'), sample('m2')] });
  buttons(settings.render(), 'Recordings')[0].props.onClick();
  await settle();
  buttons(settings.render(), 'Remove')[1].props.onClick();
  await settle();
  const removal = settings.calls.find(call => call.command === 'remove_speaker_profile_sample');
  assert.equal(JSON.stringify(removal.args.request), JSON.stringify({ profileId: 'p1', meetingId: 'm2', speakerId: 'S1' }));
  assert.equal(settings.dialogs.some(dialog => dialog.kind === 'confirm'), false);
  assert.ok(settings.calls.filter(call => call.command === 'list_speaker_profiles').length >= 1);
  const tree = settings.render();
  assert.ok(text(tree).includes('Meeting m1'));
  assert.equal(text(tree).includes('Meeting m2'), false);
}
// Removing the last recording asks first, and does nothing if declined.
{
  const settings = makeSettings({ samples: [sample('m1')], confirmAnswer: false });
  buttons(settings.render(), 'Recordings')[0].props.onClick();
  await settle();
  buttons(settings.render(), 'Remove')[0].props.onClick();
  await settle();
  assert.equal(settings.dialogs.filter(dialog => dialog.kind === 'confirm').length, 1);
  assert.equal(settings.calls.some(call => call.command === 'remove_speaker_profile_sample'), false);
}
// Confirming forgets the voice and closes its recordings.
{
  const settings = makeSettings({ samples: [sample('m1')], removeOutcome: 'forgotten' });
  buttons(settings.render(), 'Recordings')[0].props.onClick();
  await settle();
  buttons(settings.render(), 'Remove')[0].props.onClick();
  await settle();
  assert.ok(settings.calls.some(call => call.command === 'remove_speaker_profile_sample'));
  assert.equal(settings.states.get('expandedProfileId'), null);
}
// A recording that can't be separated from the blend can't be removed.
{
  const settings = makeSettings({ samples: [sample('m1'), sample('m9', { removable: false })] });
  buttons(settings.render(), 'Recordings')[0].props.onClick();
  await settle();
  const [first, second] = buttons(settings.render(), 'Remove');
  assert.equal(first.props.disabled, false);
  assert.equal(second.props.disabled, true);
  assert.ok(text(settings.render()).includes('Part of the original voice'));
}
console.log('Remembered voice recordings: all checks passed');
