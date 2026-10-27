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
const sourcePath = path.join(root, 'src/components/RememberedVoices.tsx');
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

const madior = { id: 'p1', name: 'Scrum Master Madior', modelVersion: 'v1', sampleCount: 27, speechSeconds: 60, recordingCount: 2 };
const andrew = { id: 'p2', name: 'Andrew (Me)', modelVersion: 'v1', sampleCount: 4, speechSeconds: 17.6, recordingCount: 1 };
const sample = (meetingId, extra = {}) => ({
  meetingId, speakerId: 'S1', meetingTitle: `Meeting ${meetingId}`, meetingCreatedAt: '2026-10-14T13:21:05Z',
  speakerLabel: 'Scrum Master Madior', speechSeconds: 30, removable: true, ...extra,
});

function makeSettings({ samples, confirmAnswer = true, removeOutcome = 'recalculated', profiles = [madior], rename = async () => {} }) {
  let cursor = 0;
  const states = new Map();
  const calls = [];
  const dialogs = [];
  const routes = [];
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
      if (command === 'list_speaker_profiles') return profiles;
      if (command === 'list_speaker_profile_samples') return currentSamples;
      if (command === 'update_speaker_profile') return rename(args.request);
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
    if (name === 'next/navigation') return { useRouter: () => ({ push: url => routes.push(url) }) };
    return new Proxy({}, { get: (_, key) => String(key) });
  };
  const module = { exports: {} };
  vm.runInNewContext(compiled.outputText, { exports: module.exports, module, require: mockRequire, console, Number, Date });
  states.set('profiles', profiles);
  const render = () => { cursor = 0; return module.exports.RememberedVoices(); };
  return { render, states, calls, dialogs, routes };
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
const toggles = tree => nodes(tree).filter(node => node.type === 'button' && 'aria-expanded' in node.props);
const open = (settings, index = 0) => toggles(settings.render())[index].props.onClick();
const renameButton = (tree, name = madior.name) => nodes(tree)
  .find(node => node.type === 'Button' && node.props['aria-label'] === `Rename ${name}`);
const nameInput = tree => nodes(tree).find(node => node.type === 'Input' && node.props.id === 'remembered-voice-name');
const submitRename = settings => nodes(settings.render()).find(node => node.type === 'form')
  .props.onSubmit({ preventDefault() {} });
async function settle() { for (let i = 0; i < 20; i++) await Promise.resolve(); }

// Rename starts with the current name and saves the trimmed name for the selected voice.
{
  const settings = makeSettings({ samples: [], profiles: [andrew, madior] });
  renameButton(settings.render()).props.onClick();
  assert.equal(nameInput(settings.render()).props.value, madior.name);
  assert.equal(buttons(settings.render(), 'Save')[0].props.disabled, true);
  nameInput(settings.render()).props.onChange({ target: { value: '  Aaron  ' } });
  assert.equal(buttons(settings.render(), 'Save')[0].props.disabled, false);
  submitRename(settings);
  await settle();
  const request = settings.calls.find(call => call.command === 'update_speaker_profile').args.request;
  assert.equal(JSON.stringify(request), JSON.stringify({ profileId: 'p1', name: 'Aaron' }));
  assert.equal(nameInput(settings.render()), undefined);
  const renamed = settings.states.get('profiles').find(profile => profile.id === madior.id);
  assert.equal(JSON.stringify(renamed), JSON.stringify({ ...madior, name: 'Aaron' }));
  assert.equal(settings.states.get('profiles')[1].name, andrew.name);
  assert.ok(renameButton(settings.render(), 'Aaron'));
  assert.equal(settings.calls.length, 1);
}
// Cancel and dialog dismissal discard edits. Reopening restores the saved name.
{
  const settings = makeSettings({ samples: [] });
  for (const dismiss of [
    tree => buttons(tree, 'Cancel')[0].props.onClick(),
    tree => nodes(tree).find(node => node.type === 'Dialog').props.onOpenChange(false),
  ]) {
    renameButton(settings.render()).props.onClick();
    nameInput(settings.render()).props.onChange({ target: { value: 'Changed' } });
    dismiss(settings.render());
    assert.equal(nameInput(settings.render()), undefined);
    assert.equal(settings.calls.length, 0);
    assert.equal(settings.states.get('profiles')[0].name, madior.name);
  }
  renameButton(settings.render()).props.onClick();
  assert.equal(nameInput(settings.render()).props.value, madior.name);
}
// A blank name cannot be submitted, including with the Enter key.
{
  const settings = makeSettings({ samples: [] });
  renameButton(settings.render()).props.onClick();
  nameInput(settings.render()).props.onChange({ target: { value: ' \t ' } });
  assert.equal(buttons(settings.render(), 'Save')[0].props.disabled, true);
  submitRename(settings);
  await settle();
  assert.equal(settings.calls.length, 0);
}
// A failed save keeps the old name and the edit, shows the error, and permits a retry.
{
  let attempts = 0;
  const settings = makeSettings({ samples: [], rename: async () => {
    if (++attempts === 1) throw new Error('Database unavailable');
  } });
  renameButton(settings.render()).props.onClick();
  nameInput(settings.render()).props.onChange({ target: { value: 'Madior' } });
  submitRename(settings);
  await settle();
  assert.equal(settings.states.get('profiles')[0].name, madior.name);
  assert.equal(nameInput(settings.render()).props.value, 'Madior');
  assert.ok(text(nodes(settings.render()).find(node => node.props?.role === 'alert')).includes('Database unavailable'));
  assert.equal(buttons(settings.render(), 'Save')[0].props.disabled, false);
  submitRename(settings);
  await settle();
  assert.equal(settings.states.get('profiles')[0].name, 'Madior');
  assert.equal(nameInput(settings.render()), undefined);
}
// A pending save blocks duplicate writes, dismissal, and voice removal.
{
  let finish;
  const settings = makeSettings({ samples: [], rename: () => new Promise(resolve => { finish = resolve; }) });
  renameButton(settings.render()).props.onClick();
  nameInput(settings.render()).props.onChange({ target: { value: 'Madior' } });
  submitRename(settings);
  await settle();
  let tree = settings.render();
  assert.equal(buttons(tree, 'Saving…')[0].props.disabled, true);
  assert.equal(buttons(tree, 'Cancel')[0].props.disabled, true);
  assert.equal(nameInput(tree).props.disabled, true);
  assert.equal(renameButton(tree).props.disabled, true);
  assert.equal(nodes(tree).find(node => node.props?.title === 'Forget voice').props.disabled, true);
  nodes(tree).find(node => node.type === 'Dialog').props.onOpenChange(false);
  submitRename(settings);
  assert.equal(settings.calls.filter(call => call.command === 'update_speaker_profile').length, 1);
  assert.ok(nameInput(settings.render()));
  finish();
  await settle();
  assert.equal(nameInput(settings.render()), undefined);
}
// Search uses the new name after a save.
{
  const settings = makeSettings({ samples: [] });
  renameButton(settings.render()).props.onClick();
  nameInput(settings.render()).props.onChange({ target: { value: 'Alice' } });
  submitRename(settings);
  await settle();
  const search = () => nodes(settings.render()).find(node => node.props?.type === 'search');
  search().props.onChange({ target: { value: 'Madior' } });
  assert.equal(toggles(settings.render()).length, 0);
  search().props.onChange({ target: { value: 'Alice' } });
  assert.equal(toggles(settings.render()).length, 1);
}

// Opening a voice lists the meetings that make it up.
{
  const settings = makeSettings({ samples: [sample('m1'), sample('m2')] });
  open(settings);
  await settle();
  const tree = settings.render();
  assert.ok(text(tree).includes('Meeting m1'));
  assert.ok(text(tree).includes('Meeting m2'));
  assert.equal(buttons(tree, 'Remove').length, 2);
}
// Removing one of several rebuilds the voice without asking, and refreshes the list.
{
  const settings = makeSettings({ samples: [sample('m1'), sample('m2')] });
  open(settings);
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
  open(settings);
  await settle();
  buttons(settings.render(), 'Remove')[0].props.onClick();
  await settle();
  assert.equal(settings.dialogs.filter(dialog => dialog.kind === 'confirm').length, 1);
  assert.equal(settings.calls.some(call => call.command === 'remove_speaker_profile_sample'), false);
}
// Confirming forgets the voice and closes its recordings.
{
  const settings = makeSettings({ samples: [sample('m1')], removeOutcome: 'forgotten' });
  open(settings);
  await settle();
  buttons(settings.render(), 'Remove')[0].props.onClick();
  await settle();
  assert.ok(settings.calls.some(call => call.command === 'remove_speaker_profile_sample'));
  assert.equal(settings.states.get('expandedProfileId'), null);
}
// A recording that can't be separated from the blend can't be removed.
{
  const settings = makeSettings({ samples: [sample('m1'), sample('m9', { removable: false })] });
  open(settings);
  await settle();
  const [first, second] = buttons(settings.render(), 'Remove');
  assert.equal(first.props.disabled, false);
  assert.equal(second.props.disabled, true);
  assert.ok(text(settings.render()).includes('Part of the original voice'));
}
// Rows show how much speech and how many meetings make up each voice.
{
  const settings = makeSettings({ samples: [], profiles: [andrew, madior] });
  const content = text(settings.render());
  assert.ok(content.includes('1m of speech · 2 meetings'));
  assert.ok(content.includes('18s of speech · 1 meeting'));
}
// Searching filters names by every word typed, in any order and case.
{
  const settings = makeSettings({ samples: [], profiles: [andrew, madior] });
  const search = tree => nodes(tree).find(node => node.type === 'Input');
  search(settings.render()).props.onChange({ target: { value: 'MADIOR scrum' } });
  let content = text(settings.render());
  assert.ok(content.includes('Scrum Master Madior'));
  assert.equal(content.includes('Andrew (Me)'), false);
  search(settings.render()).props.onChange({ target: { value: 'nobody' } });
  content = text(settings.render());
  assert.ok(content.includes('No remembered voices match'));
  assert.equal(toggles(settings.render()).length, 0);
}
// Forgetting a voice asks first, and keeps it if declined.
{
  const declined = makeSettings({ samples: [], confirmAnswer: false });
  const forget = tree => nodes(tree).find(node => node.type === 'Button' && node.props['aria-label']?.startsWith('Forget'));
  forget(declined.render()).props.onClick();
  await settle();
  assert.equal(declined.calls.some(call => call.command === 'delete_speaker_profile'), false);
  assert.equal(declined.states.get('profiles').length, 1);

  const accepted = makeSettings({ samples: [] });
  forget(accepted.render()).props.onClick();
  await settle();
  assert.ok(accepted.calls.some(call => call.command === 'delete_speaker_profile'));
  assert.equal(accepted.states.get('profiles').length, 0);
}
// A recording's meeting title opens that meeting.
{
  const settings = makeSettings({ samples: [sample('m1')] });
  open(settings);
  await settle();
  nodes(settings.render()).find(node => node.type === 'button' && text(node) === 'Meeting m1').props.onClick();
  assert.equal(JSON.stringify(settings.routes), JSON.stringify(['/meeting-details?id=m1']));
}
// With no voices, the empty state explains how to add one and hides search.
{
  const settings = makeSettings({ samples: [], profiles: [] });
  const tree = settings.render();
  assert.ok(text(tree).includes('No remembered voices yet'));
  assert.equal(nodes(tree).some(node => node.type === 'Input'), false);
}
console.log('Remembered voice recordings: all checks passed');
