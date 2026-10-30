import React from 'react';
import { afterAll, expect, mock, test } from 'bun:test';
import { act, create } from 'react-test-renderer';

const config = { ...await import('../../src/contexts/ConfigContext') };
const recording = { ...await import('../../src/contexts/RecordingStateContext') };
const childPaths = ['PreferenceSettings', 'DeviceSelection', 'LanguageSelection', 'TranscriptSettings'];
const originalChildren = await Promise.all(childPaths.map(name => import(`../../src/components/${name}`)));
for (const name of childPaths) mock.module(`../../src/components/${name}`, () => ({ [name]: () => null }));
const models = [
  { id: 'one', name: 'first-model', size: '1 GB', modified: 'today' },
  { id: 'two', name: 'second-model', size: '2 GB', modified: 'yesterday' },
];
let selected = 'first-model';
const setModelConfig = mock((update: any) => { selected = update({ provider: 'ollama', model: selected }).model; });
mock.module('../../src/contexts/ConfigContext', () => ({ useConfig: () => ({
  modelConfig: { provider: 'ollama', model: selected }, setModelConfig, models,
  modelOptions: { ollama: models.map(model => model.name) },
}) }));
mock.module('../../src/contexts/RecordingStateContext', () => ({ useRecordingState: () => ({ isRecording: false }) }));
const { SettingsModals } = await import('../../src/app/_components/SettingsModal');
afterAll(() => {
  mock.module('../../src/contexts/ConfigContext', () => config);
  mock.module('../../src/contexts/RecordingStateContext', () => recording);
  childPaths.forEach((name, index) => mock.module(`../../src/components/${name}`, () => originalChildren[index]));
});

test('each legacy Ollama card is one button that includes its size and modified date', async () => {
  const view = () => <SettingsModals modals={{ modelSettings: true, deviceSettings: false, languageSettings: false,
    modelSelector: false, errorAlert: false, chunkDropWarning: false }}
    messages={{ errorAlert: '', chunkDropWarning: '', modelSelector: '' }} onClose={() => {}} />;
  let renderer: ReturnType<typeof create>;
  await act(async () => { renderer = create(view()); });
  try {
    for (const model of [...models].reverse()) {
      const fields = [`Size: ${model.size}`, `Modified: ${model.modified}`].map(text =>
        renderer!.root.findAll(node => typeof node.type === 'string').find(node => node.children.join('') === text)!);
      const buttons = fields.map(field => {
        let node = field.parent;
        while (node && node.type !== 'button') node = node.parent;
        return node;
      });
      const card = buttons[0];
      expect(card).toBeDefined();
      expect(card).not.toBeNull();
      expect(buttons[1]).toBe(card);
      expect(card!.props.type).toBe('button');
      await act(async () => { card!.props.onClick(); renderer!.update(view()); });
      expect(selected).toBe(model.name);
      expect(card!.props['aria-pressed']).toBe(true);
    }
  } finally { await act(async () => renderer!.unmount()); }
});
