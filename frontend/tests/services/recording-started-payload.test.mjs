import assert from 'node:assert/strict';
import fs from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
import ts from 'typescript';

test('onRecordingStarted passes the event payload, including the recording folder', async () => {
  const source = fs.readFileSync(new URL('../../src/services/recordingService.ts', import.meta.url), 'utf8');
  const listeners = new Map();
  const require = name => {
    if (name === '@tauri-apps/api/event') {
      return { listen: async (event, handler) => { listeners.set(event, handler); return () => {}; } };
    }
    return new Proxy({}, { get: () => () => {} });
  };
  const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS } });
  const module = { exports: {} };
  vm.runInNewContext(compiled.outputText, { module, exports: module.exports, require, console, setTimeout, clearTimeout });

  const received = [];
  await new module.exports.RecordingService().onRecordingStarted(payload => received.push(payload));
  listeners.get('recording-started')({ payload: { message: 'started', folder_path: '/recordings/Meeting' } });

  assert.deepEqual(received, [{ message: 'started', folder_path: '/recordings/Meeting' }]);
});
