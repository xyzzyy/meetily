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
      betaFeatures: { importAndRetranscribe: true },
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

const attribute = tree => nodes(tree).find(node => node.type === 'SpeakerAttributeButton');

// Attributing shows pending state, prevents duplicate commands, and permits a
// retry after a failure. It never goes through the old rename path.
{
  let reject;
  const pending = new Promise((_, fail) => { reject = fail; });
  const panel = makePanel({ speakers: [alice] }, name => name === 'attribute_speaker' ? pending : []);
  attribute(panel.render()).props.onAttribute(alice, { name: 'Alice' });
  const busy = attribute(panel.render());
  assert.equal(busy.props.busy, true);
  assert.equal(busy.props.disabled, true);
  busy.props.onAttribute(alice, { name: 'Alice' });
  assert.equal(panel.calls.filter(call => call.name === 'attribute_speaker').length, 1);
  assert.equal(JSON.stringify(panel.calls[0].args.request), JSON.stringify({ meetingId: 'meeting', speakerId: 'alice', name: 'Alice' }));
  reject(new Error('Speaker not found'));
  await settle();
  assert.equal(attribute(panel.render()).props.disabled, false);
  assert.ok(panel.notices.some(notice => notice.kind === 'error'));
  attribute(panel.render()).props.onAttribute(alice, { name: 'Alice' });
  await settle();
  assert.equal(panel.calls.filter(call => call.name === 'attribute_speaker').length, 2);
  assert.equal(panel.calls.some(call => call.name === 'rename_speaker' || call.name === 'remember_speaker'), false);
}
// Picking a remembered voice sends its id, relabels the speaker, and reloads voices.
{
  const madior = { id: 'profile-1', name: 'Scrum Master Madior', modelVersion: 'v1', sampleCount: 3, speechSeconds: 30 };
  const panel = makePanel({ speakers: [alice], speakerProfiles: [madior] }, async name => name === 'attribute_speaker'
    ? { profileId: madior.id, name: madior.name, outcome: 'added' } : []);
  assert.deepEqual(attribute(panel.render()).props.profiles, [madior]);
  attribute(panel.render()).props.onAttribute(alice, { profileId: madior.id });
  await settle();
  const call = panel.calls.find(entry => entry.name === 'attribute_speaker');
  assert.equal(JSON.stringify(call.args.request), JSON.stringify({ meetingId: 'meeting', speakerId: 'alice', profileId: madior.id }));
  assert.ok(panel.calls.some(entry => entry.name === 'list_speaker_profiles'));
  assert.ok(panel.notices.some(notice => notice.kind === 'success'
    && String(notice.args[0]).includes(madior.name)));
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
  assert.equal(nodes(tree).find(node => node.type === 'SpeakerColorPicker').props.disabled, true);
  for (const label of ['Merge', 'Apply (1)']) {
    const action = button(tree, label);
    assert.equal(action.props.disabled, true, `${label} enabled during ${currentStatus}`);
    action.props.onClick();
  }
  for (const action of nodes(tree).filter(node => node.type === 'SpeakerAttributeButton')) {
    assert.equal(action.props.disabled, true, `Attribute enabled during ${currentStatus}`);
    action.props.onAttribute(action.props.speaker, { name: 'Someone' });
  }
  await settle();
  assert.equal(panel.calls.length, 0);
  assert.equal(nodes(tree).find(node => node.type === 'VirtualizedTranscriptView').props.onToggleSegment, undefined);
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
  assert.equal(attribute(tree).props.disabled, true);
  assert.equal(nodes(tree).some(node => node.type === 'Button' && text(node) === 'Retry'), false);
  assert.ok(content.includes('Close and reopen the app'));
  assert.ok(content.includes('Transcript recovery is required'));
  assert.equal(content.includes('Using the live transcript'), false);
}
// Collapsing hides the controls and ends row selection. Correction reopens them.
{
  const panel = makePanel({ speakers: [alice], correctionMode: true,
    selectedRowIds: new Set(['r1']), diarizationSettings: { enabled: true },
    diarizationStatus: { currentStatus: 'completed', progress: 100 } });
  const toggle = tree => nodes(tree).find(node => node.props['aria-controls'] === 'speaker-controls-meeting');
  const body = tree => nodes(tree).find(node => node.props.id === 'speaker-controls-meeting');
  assert.equal(toggle(panel.render()).props['aria-expanded'], true);
  toggle(panel.render()).props.onClick();
  const collapsed = panel.render();
  assert.equal(toggle(collapsed).props['aria-expanded'], false);
  assert.equal(body(collapsed).props.hidden, true);
  assert.equal(panel.states.get('selectedRowIds').size, 0);
  assert.equal(panel.states.get('correctionMode'), false);
  button(collapsed, 'Run again');
  button(collapsed, 'Correct rows').props.onClick();
  assert.equal(body(panel.render()).props.hidden, false);
  assert.equal(panel.states.get('correctionMode'), true);
}
// A saved color updates both the list and the visible transcript, without renaming.
{
  const blueAlice = { ...alice, speakerColor: '#2563eb' };
  const panel = makePanel({ speakers: [blueAlice] }, async () => [], {
    transcripts: [{ id: 'r1', text: 'Example', speaker_id: 'alice', speaker_color: '#2563eb' }],
  });
  const picker = tree => nodes(tree).find(node => node.type === 'SpeakerColorPicker');
  assert.equal(await picker(panel.render()).props.onChange('#be185d'), true);
  assert.equal(picker(panel.render()).props.color, '#be185d');
  const transcript = nodes(panel.render()).find(node => node.type === 'VirtualizedTranscriptView');
  assert.equal(transcript.props.segments[0].speaker_color, '#be185d');
  assert.equal(panel.calls[0].name, 'set_speaker_color');
  assert.equal(panel.calls[0].args.request.speakerId, 'alice');
  assert.equal(panel.calls.some(call => call.name === 'rename_speaker'), false);
}
{
  const panel = makePanel({ speakers: [{ ...alice, speakerColor: '#2563eb' }] },
    async () => { throw new Error('Save failed'); });
  const picker = nodes(panel.render()).find(node => node.type === 'SpeakerColorPicker');
  assert.equal(await picker.props.onChange('#be185d'), false);
  assert.equal(panel.states.get('speakers')[0].speakerColor, '#2563eb');
  assert.equal(panel.states.get('savingSpeakerId'), null);
}
console.log('Speaker review controls: all checks passed');

// A speaker summary is confirmed only when every row is confirmed. Use the
// full-meeting counts, including rows that are outside the loaded transcript page.
{
  const panel = makePanel({ speakers: [
    { ...alice, segmentCount: 2, verifiedSegmentCount: 1 },
    { ...alice, speakerId: 'bob', speakerLabel: 'Bob', verifiedSegmentCount: 1 },
    { ...alice, speakerId: 'unknown', speakerLabel: 'Speaker 3' },
  ] });
  const indicators = nodes(panel.render()).filter(node => node.type === 'SpeakerAttributionIndicator');
  assert.deepEqual(indicators.map(node => node.props.verified), [false, true, false]);
  assert.match(indicators[0].props.description, /1 of 2 transcript segments confirmed/);
}

// A label-only rejection never starts retranscription. Enhance is a separate,
// explicit action which opens the existing dialog without starting a job.
{
  const message = 'Speaker analysis did not cover enough of the existing transcript';
  const panel = makePanel({ diarizationSettings: { enabled: true },
    diarizationStatus: { currentStatus: 'completed', progress: 100 } }, async name => {
    if (name === 'retry_final_diarization') throw new Error(message);
    return [];
  });
  button(panel.render(), 'Run again').props.onClick();
  await settle();
  assert.deepEqual(panel.calls.map(call => call.name), ['retry_final_diarization']);
  assert.equal(panel.states.get('enhanceDialogOpen'), false);
  assert.ok(text(panel.render()).includes('The saved transcript and labels were kept.'));
  button(panel.render(), 'Enhance').props.onClick();
  const dialog = nodes(panel.render()).find(node => node.type === 'RetranscribeDialog');
  assert.equal(dialog.props.open, true);
  assert.equal(dialog.props.meetingId, 'meeting');
  assert.equal(panel.calls.some(call => call.name === 'start_retranscription_command'), false);
}
