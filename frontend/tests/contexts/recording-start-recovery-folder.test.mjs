import assert from 'node:assert/strict';
import fs from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
import ts from 'typescript';

const folder = '/recordings/Standup';

/** Run the provider's real recording-started listener and record each IndexedDB write. */
async function startRecording(payload) {
  const source = fs.readFileSync(new URL('../../src/contexts/TranscriptContext.tsx', import.meta.url), 'utf8');
  const file = ts.createSourceFile('TranscriptContext.tsx', source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  let initializer;
  function visit(node) {
    if (ts.isVariableDeclaration(node) && node.name.getText(file) === 'setupRecordingListeners') {
      initializer = node.initializer.getText(file);
    }
    ts.forEachChild(node, visit);
  }
  visit(file);
  assert.ok(initializer);

  const writes = [];
  const records = new Map();
  let onStarted;
  const compiled = ts.transpileModule(
    `let unlistenRecordingStarted, unlistenRecordingStopped; const setup = ${initializer}; setup;`,
    { compilerOptions: { module: ts.ModuleKind.CommonJS } },
  ).outputText;
  const setup = vm.runInNewContext(compiled, {
    indexedDBService: {
      init: async () => {},
      saveMeetingMetadata: async metadata => {
        writes.push({ ...metadata });
        records.set(metadata.meetingId, { ...metadata });
      },
      getMeetingMetadata: async id => records.get(id) && { ...records.get(id) },
    },
    recordingService: {
      onRecordingStarted: async callback => { onStarted = callback; return () => {}; },
      onRecordingStopped: async () => () => {},
      getRecordingMeetingName: async () => 'Standup',
    },
    // Older code asked the backend for the active folder after the first write.
    require: name => {
      assert.equal(name, '@tauri-apps/api/core');
      return { invoke: async command => (command === 'get_meeting_folder_path' ? folder : undefined) };
    },
    setCurrentMeetingId: () => {},
    setMeetingTitle: () => {},
    currentMeetingId: null,
    sessionStorage: { setItem: () => {} },
    console: { log: () => {}, error: error => assert.fail(`Unexpected listener error: ${error}`) },
  });
  await setup();
  assert.equal(typeof onStarted, 'function');
  await onStarted(payload);
  return writes;
}

test('the first recovery record already holds the recording folder', async () => {
  const writes = await startRecording({ message: 'Started', folder_path: folder });
  // A crash after the first write leaves only this record for recovery.
  assert.equal(writes[0].folderPath, folder, 'The first recovery record was saved without the recording folder');
  assert.equal(writes[0].title, 'Standup');
  assert.equal(writes.every(write => write.folderPath === folder), true);
});

test('a start event without a folder saves a record without one', async () => {
  const writes = await startRecording({ message: 'Started', folder_path: null });
  assert.equal(writes[0].folderPath, undefined);
});
