import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { runInNewContext } from 'node:vm';
import { ThemeProvider, useTheme } from '../../src/contexts/ThemeContext';
import { THEME_STORAGE_KEY, themeInitScript } from '../../src/lib/theme';

const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
const originalDocument = Object.getOwnPropertyDescriptor(globalThis, 'document');
const originalTauri = Object.getOwnPropertyDescriptor(globalThis, 'isTauri');
let current: ReturnType<typeof useTheme>;
let renderer: ReactTestRenderer | undefined;
let systemDark: boolean;
let dark: boolean;
let saved: string | null;
let storageBlocked: boolean;
let nativeThemes: (string | null)[];
let style: { colorScheme?: string };
let systemListeners: Set<() => void>;
let storageListeners: Set<(event: { key: string | null; newValue: string | null }) => void>;

function ReadTheme() {
  current = useTheme();
  return null;
}

function mount() {
  act(() => {
    renderer = create(<ThemeProvider><ReadTheme /></ThemeProvider>);
  });
}

beforeEach(() => {
  systemDark = false;
  dark = false;
  saved = null;
  storageBlocked = false;
  nativeThemes = [];
  style = {};
  systemListeners = new Set();
  storageListeners = new Set();
  Object.defineProperty(globalThis, 'isTauri', { configurable: true, value: false });
  Object.defineProperty(globalThis, 'document', {
    configurable: true,
    value: { documentElement: { style, classList: { toggle: (_: string, enabled: boolean) => { dark = enabled; } } } },
  });
  Object.defineProperty(globalThis, 'window', {
    configurable: true,
    value: {
      __TAURI_INTERNALS__: {
        metadata: { currentWindow: { label: 'main' } },
        invoke: async (command: string, args: { value: string | null }) => {
          expect(command).toBe('plugin:window|set_theme');
          nativeThemes.push(args.value);
        },
      },
      localStorage: {
        getItem: () => {
          if (storageBlocked) throw new Error('Storage blocked');
          return saved;
        },
        setItem: (key: string, value: string) => {
          expect(key).toBe(THEME_STORAGE_KEY);
          if (storageBlocked) throw new Error('Storage blocked');
          saved = value;
        },
      },
      matchMedia: () => ({
        matches: systemDark,
        addEventListener: (_: string, listener: () => void) => systemListeners.add(listener),
        removeEventListener: (_: string, listener: () => void) => systemListeners.delete(listener),
      }),
      addEventListener: (_: string, listener: (event: { key: string | null; newValue: string | null }) => void) => storageListeners.add(listener),
      removeEventListener: (_: string, listener: (event: { key: string | null; newValue: string | null }) => void) => storageListeners.delete(listener),
    },
  });
});

afterEach(() => {
  act(() => renderer?.unmount());
  renderer = undefined;
  if (originalWindow) Object.defineProperty(globalThis, 'window', originalWindow);
  else Reflect.deleteProperty(globalThis, 'window');
  if (originalDocument) Object.defineProperty(globalThis, 'document', originalDocument);
  else Reflect.deleteProperty(globalThis, 'document');
  if (originalTauri) Object.defineProperty(globalThis, 'isTauri', originalTauri);
  else Reflect.deleteProperty(globalThis, 'isTauri');
});

describe('theme preference', () => {
  test('uses the system theme by default and follows system changes', () => {
    systemDark = true;
    mount();
    expect(current.theme).toBe('system');
    expect(current.resolvedTheme).toBe('dark');
    expect(dark).toBe(true);
    expect(style.colorScheme).toBe('dark');

    act(() => {
      systemDark = false;
      systemListeners.forEach(listener => listener());
    });
    expect(current.resolvedTheme).toBe('light');
    expect(dark).toBe(false);
  });

  test('saves a manual choice and restores it after a restart', () => {
    mount();
    act(() => current.setTheme('dark'));
    expect(saved).toBe('dark');
    expect(dark).toBe(true);
    act(() => {
      systemListeners.forEach(listener => listener());
    });
    expect(current.resolvedTheme).toBe('dark');

    act(() => renderer?.unmount());
    mount();
    expect(current.theme).toBe('dark');
    expect(dark).toBe(true);

    act(() => current.setTheme('light'));
    expect(saved).toBe('light');
    expect(style.colorScheme).toBe('light');
    expect(dark).toBe(false);
  });

  test('resumes system updates after the user selects System', () => {
    saved = 'light';
    systemDark = true;
    mount();
    expect(dark).toBe(false);
    act(() => current.setTheme('system'));
    expect(current.theme).toBe('system');
    expect(current.resolvedTheme).toBe('dark');
    expect(saved).toBe('system');
  });

  test('accepts changes from another window and removes listeners on unmount', () => {
    mount();
    act(() => {
      storageListeners.forEach(listener => listener({ key: THEME_STORAGE_KEY, newValue: 'dark' }));
    });
    expect(current.theme).toBe('dark');
    expect(dark).toBe(true);
    act(() => {
      storageListeners.forEach(listener => listener({ key: null, newValue: null }));
    });
    expect(current.theme).toBe('system');
    expect(dark).toBe(false);
    act(() => renderer?.unmount());
    renderer = undefined;
    expect(systemListeners.size).toBe(0);
    expect(storageListeners.size).toBe(0);
  });

  test('still applies a selection when local storage is blocked', () => {
    storageBlocked = true;
    systemDark = true;
    mount();
    expect(dark).toBe(true);
    act(() => current.setTheme('light'));
    expect(current.theme).toBe('light');
    expect(dark).toBe(false);
  });

  test('ignores an invalid saved value', () => {
    saved = 'invalid';
    systemDark = true;
    mount();
    expect(current.theme).toBe('system');
    expect(dark).toBe(true);
  });

  test('keeps the desktop window theme in sync with the selected preference', () => {
    Object.defineProperty(globalThis, 'isTauri', { configurable: true, value: true });
    mount();
    act(() => current.setTheme('dark'));
    act(() => current.setTheme('light'));
    act(() => current.setTheme('system'));
    expect(nativeThemes).toEqual([null, 'dark', 'light', null]);
  });
});

describe('theme before the first paint', () => {
  for (const [preference, prefersDark, blocked, expectedDark] of [
    ['dark', false, false, true],
    ['light', true, false, false],
    ['system', true, false, true],
    [null, false, false, false],
    ['invalid', true, false, true],
    [null, true, true, true],
  ] as const) {
    test(`applies ${preference ?? 'default'} with system dark=${prefersDark}, storage blocked=${blocked}`, () => {
      saved = preference;
      systemDark = prefersDark;
      storageBlocked = blocked;
      runInNewContext(themeInitScript, {
        localStorage: window.localStorage,
        matchMedia: window.matchMedia,
        document,
      });
      expect(dark).toBe(expectedDark);
      expect(style.colorScheme).toBe(expectedDark ? 'dark' : 'light');
    });
  }
});
