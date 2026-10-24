import assert from 'node:assert/strict';
import fs from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
import ts from 'typescript';
import React from 'react';
import { act, create } from 'react-test-renderer';

/**
 * Load the real hook with a fake Tauri `listen` whose registrations finish only
 * when the test resolves them, so every timing can be exercised.
 */
function load() {
  const live = new Set();
  const pending = [];
  const errors = [];
  const listen = (event, handler) => new Promise((resolve, reject) => {
    pending.push({
      event,
      resolve: () => {
        const entry = { event, handler };
        live.add(entry);
        resolve(() => live.delete(entry));
      },
      reject: () => reject(new Error(`cannot listen for ${event}`)),
    });
  });
  const source = fs.readFileSync(new URL('../../src/hooks/useTauriEvent.ts', import.meta.url), 'utf8');
  const module = { exports: {} };
  vm.runInNewContext(ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS } }).outputText, {
    module, exports: module.exports,
    console: { error: (...args) => errors.push(args.join(' ')) },
    require: name => (name === 'react' ? React : { listen }),
  });
  const settle = () => act(async () => { await new Promise(resolve => setImmediate(resolve)); });
  const take = event => {
    const index = pending.findIndex(entry => entry.event === event);
    assert.notEqual(index, -1, `a registration for ${event} is pending`);
    return pending.splice(index, 1)[0];
  };
  const emit = (event, payload) => [...live].filter(entry => entry.event === event).forEach(entry => entry.handler({ payload }));
  return { useTauriEvent: module.exports.useTauriEvent, live, pending, errors, settle, take, emit };
}

function Listener({ hook, event, onEvent }) {
  hook(event, received => onEvent(received.payload));
  return null;
}

test('a listener that registers after unmount is removed at once and never called', async () => {
  const app = load();
  const received = [];
  let renderer;
  await act(async () => {
    renderer = create(React.createElement(Listener, { hook: app.useTauriEvent, event: 'speech-detected', onEvent: value => received.push(value) }));
  });
  await act(async () => renderer.unmount());
  app.take('speech-detected').resolve();
  await app.settle();
  assert.equal(app.live.size, 0);
  app.emit('speech-detected', 'late');
  assert.deepEqual(received, []);
});

test('a failed registration does not affect another listener', async () => {
  const app = load();
  const received = [];
  const Both = () => [
    React.createElement(Listener, { key: 'a', hook: app.useTauriEvent, event: 'transcription-error', onEvent() {} }),
    React.createElement(Listener, { key: 'b', hook: app.useTauriEvent, event: 'speech-detected', onEvent: value => received.push(value) }),
  ];
  let renderer;
  await act(async () => { renderer = create(React.createElement(Both)); });
  app.take('transcription-error').reject();
  app.take('speech-detected').resolve();
  await app.settle();
  assert.match(app.errors.join('\n'), /transcription-error/);
  app.emit('speech-detected', 'heard');
  assert.deepEqual(received, ['heard']);
  await act(async () => renderer.unmount());
  assert.equal(app.live.size, 0, 'the listener that did register is removed');
});

test('the latest handler is called and re-rendering does not register again', async () => {
  const app = load();
  const received = [];
  let renderer;
  const render = label => React.createElement(Listener, { hook: app.useTauriEvent, event: 'speech-detected', onEvent: value => received.push(`${label}:${value}`) });
  await act(async () => { renderer = create(render('first')); });
  app.take('speech-detected').resolve();
  await app.settle();
  await act(async () => renderer.update(render('second')));
  assert.equal(app.pending.length, 0, 'no new registration');
  app.emit('speech-detected', 'heard');
  assert.deepEqual(received, ['second:heard']);
  await act(async () => renderer.unmount());
});

test('changing the event name moves the listener to the new event', async () => {
  const app = load();
  let renderer;
  const render = event => React.createElement(Listener, { hook: app.useTauriEvent, event, onEvent() {} });
  await act(async () => { renderer = create(render('first-event')); });
  app.take('first-event').resolve();
  await app.settle();
  await act(async () => renderer.update(render('second-event')));
  app.take('second-event').resolve();
  await app.settle();
  assert.deepEqual([...app.live].map(entry => entry.event), ['second-event']);
  await act(async () => renderer.unmount());
  assert.equal(app.live.size, 0);
});
