import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const ts = require(process.env.TYPESCRIPT_PATH || 'typescript');
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const sourcePath = path.join(root, 'src/components/MeetingDetails/TranscriptPanel.tsx');
const source = fs.readFileSync(sourcePath, 'utf8');
const ast = ts.createSourceFile(sourcePath, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
const stateNames = [];
function visit(node) {
  if (ts.isVariableDeclaration(node) && ts.isArrayBindingPattern(node.name)
      && node.initializer && ts.isCallExpression(node.initializer)
      && node.initializer.expression.getText(ast) === 'useState') {
    stateNames.push(node.name.elements[0].name.getText(ast));
  }
  ts.forEachChild(node, visit);
}
visit(ast);

function makePanel(overrides = {}, command = async () => undefined, extraProps = {}) {
  let cursor = 0;
  const states = new Map(Object.entries(overrides));
  const settings = { enabled: false, mode: 'live_plus_post_call',
    postCallRefinementEnabled: true, ...overrides.diarizationSettings };
  const effects = [];
  const calls = [];
  const notices = [];
  const mutationRef = { current: false };
  const jsx = (type, props) => ({ type, props: props || {} });
  const hooks = {
    useState(initial) {
      const key = stateNames[cursor++];
      if (!states.has(key)) states.set(key, initial);
      return [states.get(key), value => states.set(key,
        typeof value === 'function' ? value(states.get(key)) : value)];
    },
    useRef: () => mutationRef,
    useMemo: fn => fn(),
    useCallback: fn => fn,
    useEffect: fn => { effects.push(fn); },
  };
  const mockRequire = name => {
    if (name === 'react') return hooks;
    if (name === '@/contexts/ConfigContext') return { useConfig: () => ({
      diarizationSettings: settings, isLoadingDiarizationSettings: false,
    }) };
    if (name === 'react/jsx-runtime') return { jsx, jsxs: jsx, Fragment: 'Fragment' };
    if (name === '@tauri-apps/api/core') return {
      invoke: async (name, args) => { calls.push({ name, args }); return command(name, args); },
    };
    if (name === '@tauri-apps/api/event') return { listen: async () => () => {} };
    if (name === '@tauri-apps/plugin-dialog') return { confirm: async () => true };
    if (name === 'sonner') return { toast: new Proxy({}, {
      get: (_, kind) => (...args) => notices.push({ kind, args }),
    }) };
    return new Proxy({}, { get: (_, key) => String(key) });
  };
  const compiled = ts.transpileModule(source, { compilerOptions: {
    module: ts.ModuleKind.CommonJS,
    target: ts.ScriptTarget.ES2020,
    jsx: ts.JsxEmit.ReactJSX,
  }, reportDiagnostics: true });
  assert.equal(compiled.diagnostics.length, 0);
  const module = { exports: {} };
  vm.runInNewContext(compiled.outputText, {
    exports: module.exports, module, require: mockRequire, console, Set, Date,
  });
  const props = {
    meetingId: 'meeting', meetingFolderPath: '/retained-audio', isRecording: false,
    transcripts: [{ id: 'r1', text: 'Example', audio_start_time: 0 }],
    customPrompt: '', onPromptChange() {}, onCopyTranscript() {}, onOpenMeetingFolder() {},
    ...extraProps,
  };
  const render = () => { cursor = 0; effects.length = 0; return module.exports.TranscriptPanel(props); };
  return { states, effects, calls, notices, settings, render };
}
function nodes(node) {
  if (!node || typeof node !== 'object') return [];
  if (Array.isArray(node)) return node.flatMap(nodes);
  return [node, ...nodes(node.props?.children)];
}
function text(node) {
  if (Array.isArray(node)) return node.map(text).join('');
  if (node === null || node === undefined || typeof node === 'boolean') return '';
  if (typeof node !== 'object') return String(node);
  return text(node.props?.children);
}
function button(tree, label) {
  const found = nodes(tree).find(node => node.type === 'Button' && text(node) === label);
  assert.ok(found, `Missing button: ${label}`);
  return found;
}
async function settle() { for (let i = 0; i < 15; i++) await Promise.resolve(); }
const alice = { speakerId: 'alice', speakerLabel: 'Alice', segmentCount: 1 };

// Remembering a voice must not require a name change. It must show pending state,
// prevent duplicate commands, and permit a retry after a failure.
{
  let reject;
  const pending = new Promise((_, fail) => { reject = fail; });
  const panel = makePanel({ speakers: [alice] }, name => name === 'remember_speaker' ? pending : []);
  button(panel.render(), 'Remember voice').props.onClick();
  const busy = button(panel.render(), 'Remembering…');
  assert.equal(busy.props.disabled, true);
  busy.props.onClick();
  assert.equal(panel.calls.filter(call => call.name === 'remember_speaker').length, 1);
  assert.equal(panel.calls[0].args.request.name, 'Alice');
  reject(new Error('Not enough clean speech'));
  await settle();
  assert.equal(button(panel.render(), 'Remember voice').props.disabled, false);
  button(panel.render(), 'Remember voice').props.onClick();
  await settle();
  assert.equal(panel.calls.filter(call => call.name === 'remember_speaker').length, 2);
  assert.equal(panel.calls.some(call => call.name === 'rename_speaker'), false);
}
{
  const panel = makePanel({ speakers: [alice] });
  button(panel.render(), 'Remember voice').props.onClick();
  await settle();
  assert.equal(button(panel.render(), 'Voice remembered').props.disabled, true);
}

// Overlap-only and unlabelled transcripts must keep a route back to correction.
for (const overlap of [true, false]) {
  const panel = makePanel({}, async () => [], {
    transcripts: [{ id: 'r1', text: 'Example', audio_start_time: 0, is_overlap: overlap,
      speaker_id: null, speaker_label: overlap ? 'Multiple speakers' : null }],
  });
  button(panel.render(), 'Correct rows').props.onClick();
  const editing = panel.render();
  assert.ok(nodes(editing).some(node => node.type === 'option' && node.props.value === '__new'));
  nodes(editing).find(node => node.type === 'VirtualizedTranscriptView').props.onToggleSegment('r1');
  nodes(editing).find(node => node.type === 'select').props.onChange({ target: { value: '__new' } });
  const naming = panel.render();
  nodes(naming).find(node => node.type === 'Input').props.onChange({ target: { value: 'Alice' } });
  button(panel.render(), 'Apply (1)').props.onClick();
  await settle();
  const correction = panel.calls.find(call => call.name === 'reassign_transcripts');
  assert.ok(correction, 'Rows without a speaker must still accept corrections');
  assert.equal(correction.args.request.speakerLabel, 'Alice');
  assert.equal(correction.args.request.isOverlap, false);
}

// Every mutation is disabled and guarded while refinement is active.
for (const currentStatus of ['processing', 'preparing_models', 'rebuilding_windows', 'transcribing_windows', 'saving']) {
  const panel = makePanel({
    speakers: [alice, { ...alice, speakerId: 'bob', speakerLabel: 'Bob' }],
    diarizationStatus: { currentStatus, progress: 20 },
    correctionMode: true, selectedRowIds: new Set(['r1']), assignmentTargetId: 'alice',
    mergingSpeakerId: 'bob', mergeTargetId: 'alice',
  });
  const tree = panel.render();
  assert.ok(text(tree).includes('Refining speakers locally…'));
  for (const label of ['Rename', 'Remember voice', 'Merge', 'Apply (1)']) {
    const action = button(tree, label);
    assert.equal(action.props.disabled, true, `${label} enabled during ${currentStatus}`);
    action.props.onClick();
  }
  await settle();
  assert.equal(panel.calls.length, 0);
  assert.equal(nodes(tree).find(node => node.type === 'VirtualizedTranscriptView').props.onToggleSegment, undefined);
}

// An editor that was already open must also reject Save after processing starts.
{
  const panel = makePanel({ speakers: [alice], editingSpeakerId: 'alice',
    draftSpeakerLabel: 'Alicia', diarizationStatus: { currentStatus: 'saving', progress: 95 } });
  const tree = panel.render();
  const save = button(tree, 'Save');
  assert.equal(save.props.disabled, true);
  assert.equal(nodes(tree).find(node => node.type === 'Input').props.disabled, true);
  save.props.onClick();
  await settle();
  assert.equal(panel.calls.length, 0);
}

// Manual processing follows settings changes without closing the meeting.
// It stays available when automatic processing is off.
{
  const panel = makePanel({}, async () => []);
  assert.equal(nodes(panel.render()).some(node => node.type === 'Button'
    && text(node) === 'Identify speakers'), false);
  Object.assign(panel.settings, { enabled: true, mode: 'post_call_only',
    postCallRefinementEnabled: false });
  const action = button(panel.render(), 'Identify speakers');
  assert.equal(action.props.disabled, false);
  action.props.onClick();
  await settle();
  assert.ok(panel.calls.some(call => call.name === 'start_final_diarization'));
  panel.settings.mode = 'off';
  assert.equal(nodes(panel.render()).some(node => node.type === 'Button'
    && text(node) === 'Identify speakers'), false);
  panel.settings.mode = 'live_plus_post_call';
  panel.settings.enabled = false;
  assert.equal(nodes(panel.render()).some(node => node.type === 'Button'
    && text(node) === 'Identify speakers'), false);
  assert.equal(panel.calls.some(call => call.name === 'get_diarization_settings'), false);
}
{
  const panel = makePanel({ diarizationSettings: { enabled: true }, diarizationStatus: { currentStatus: 'completed', progress: 100 } });
  const completed = panel.render();
  assert.equal(text(completed).includes('Speaker recognition complete'), false);
  assert.equal(text(completed).includes('100% complete'), false);
  button(completed, 'Run again').props.onClick();
  await settle();
  assert.ok(panel.calls.some(call => call.name === 'retry_final_diarization'));
}
{
  const panel = makePanel({ speakers: [alice], diarizationStatus: { currentStatus: 'recovery_required', progress: 100, failureReason: 'Recovery failed' } });
  const tree = panel.render();
  const content = text(tree);
  assert.equal(button(tree, 'Correct rows').props.disabled, true);
  assert.equal(button(tree, 'Remember voice').props.disabled, true);
  assert.equal(nodes(tree).some(node => node.type === 'Button' && text(node) === 'Retry'), false);
  assert.ok(content.includes('Close and reopen the app'));
  assert.ok(content.includes('Transcript recovery is required'));
  assert.equal(content.includes('Using the live transcript'), false);
}
console.log('Speaker review controls: all checks passed');
