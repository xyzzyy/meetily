import { expect, test } from 'bun:test';
import { languagesForProvider } from '../../src/lib/funasr';
import { LANGUAGES } from '../../src/constants/languages';

test('Nemotron offers runtime language hints and labels Auto as no hint', () => {
  const choices = languagesForProvider('nemotron', LANGUAGES);
  expect(choices.find(l => l.code === 'auto')?.name).toBe('Auto (No language hint)');
  expect(choices.filter(l => ['en', 'fr', 'zh', 'tl'].includes(l.code))).toHaveLength(4);
  expect(choices.some(l => l.code === 'auto-translate' || l.code === 'haw')).toBe(false);
  expect(choices).toHaveLength(42); // 41 list entries plus Auto; nb and fil are aliases.
  expect(LANGUAGES[0].name).toBe('Auto Detect (Original Language)');
  expect(languagesForProvider('funasr', LANGUAGES).map(l => l.code)).toEqual(['auto', 'en', 'zh', 'ja']);
  expect(languagesForProvider('localWhisper', LANGUAGES)).toBe(LANGUAGES);
});
