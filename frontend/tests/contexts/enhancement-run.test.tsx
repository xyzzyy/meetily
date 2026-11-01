import React from 'react';
import { afterAll, afterEach, beforeEach, expect, mock, test } from 'bun:test';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';

const core = { ...await import('@tauri-apps/api/core') };
const events = { ...await import('@tauri-apps/api/event') };
const notices = { ...await import('sonner') };
const navigation = { ...await import('next/navigation') };
const handlers = new Map<string, (event: any) => void>();
const invoke = mock(async (_command: string, _args?: unknown): Promise<any> => undefined);
const success = mock((_title: string, _options?: any) => 'toast');
const error = mock((_title: string, _options?: any) => 'toast');
const push = mock((_href: string) => {});
mock.module('@tauri-apps/api/core', () => ({ ...core, invoke }));
mock.module('@tauri-apps/api/event', () => ({ ...events, listen: async (name: string, handler: any) => {
  handlers.set(name, handler);
  return () => handlers.delete(name);
} }));
mock.module('sonner', () => ({ ...notices, toast: { success, error } }));
mock.module('next/navigation', () => ({ ...navigation, useRouter: () => ({ push }) }));
const { EnhancementRunProvider, useEnhancementRun } = await import('../../src/contexts/EnhancementRunContext');

let renderer: ReactTestRenderer;
let ctx: ReturnType<typeof useEnhancementRun>;
function Probe() { ctx = useEnhancementRun(); return null; }
const send = async (event: string, payload: any) => { await act(async () => handlers.get(event)!({ payload })); };
const progress = (percent: number) => ({ meeting_id: 'meeting', stage: 'transcribing', progress_percentage: percent, message: 'Transcribing audio...' });

beforeEach(async () => {
  handlers.clear(); invoke.mockClear(); success.mockClear(); error.mockClear(); push.mockClear();
  (globalThis as any).window = { location: { pathname: '/settings', search: '' } };
  await act(async () => { renderer = create(<EnhancementRunProvider><Probe /></EnhancementRunProvider>); });
});
afterEach(async () => { await act(async () => renderer.unmount()); });
afterAll(() => {
  delete (globalThis as any).window;
  mock.module('@tauri-apps/api/core', () => core);
  mock.module('@tauri-apps/api/event', () => events);
  mock.module('sonner', () => notices);
  mock.module('next/navigation', () => navigation);
});

test('a minimized run keeps its progress and offers to reopen the result', async () => {
  await act(async () => { ctx.openEnhancement('meeting', '/audio'); ctx.markStarted('meeting'); });
  await act(async () => ctx.setDialogOpen(false));
  await send('retranscription-progress', progress(40));
  expect(ctx.run).toMatchObject({ meetingId: 'meeting', status: 'running', progress: { progress_percentage: 40 } });

  await send('retranscription-complete', { meeting_id: 'meeting', warning: null });
  expect(ctx.run).toBeNull();
  expect(success).toHaveBeenCalledTimes(1);
  await act(async () => success.mock.calls[0][1].action.onClick());
  expect(push).toHaveBeenCalledWith('/meeting-details?id=meeting');
  expect(ctx.dialog).toEqual({ meetingId: 'meeting', meetingFolderPath: '/audio', open: true });
});

test('an open popup reports the result itself', async () => {
  await act(async () => { ctx.openEnhancement('meeting', '/audio'); ctx.markStarted('meeting'); });
  await send('retranscription-complete', { meeting_id: 'meeting', warning: null });
  expect(success).not.toHaveBeenCalled();

  await act(async () => ctx.markStarted('meeting'));
  await send('retranscription-error', { meeting_id: 'meeting', error: 'Model crashed' });
  expect(error).not.toHaveBeenCalled();
  expect(ctx.run).toBeNull();
});

test('a failure while minimized is kept for the popup', async () => {
  (globalThis as any).window.location = { pathname: '/meeting-details', search: '?id=meeting' };
  await send('retranscription-progress', progress(10));
  await send('retranscription-error', { meeting_id: 'meeting', error: 'Model crashed' });
  expect(ctx.run).toEqual({ meetingId: 'meeting', status: 'failed', progress: null, error: 'Model crashed' });
  await act(async () => error.mock.calls[0][1].action.onClick());
  expect(push).not.toHaveBeenCalled();
  expect(ctx.dialog?.open).toBe(true);
  await act(async () => ctx.dismissFailure('meeting'));
  expect(ctx.run).toBeNull();
});

test('a cancelled run keeps the meeting busy until the backend stops it', async () => {
  await act(async () => ctx.markStarted('meeting'));
  await act(async () => { expect(await ctx.cancelEnhancement()).toBe(true); });
  expect(invoke).toHaveBeenCalledWith('cancel_retranscription_command');
  expect(ctx.run?.status).toBe('running');
  await send('retranscription-progress', progress(60));
  await send('retranscription-error', { meeting_id: 'meeting', error: 'Retranscription cancelled' });
  expect(ctx.run).toBeNull();
  expect(error).not.toHaveBeenCalled();
  // Later runs are tracked again.
  await send('retranscription-progress', progress(5));
  expect(ctx.run?.status).toBe('running');
  // A run that finishes before the cancel lands ends quietly too.
  await act(async () => { expect(await ctx.cancelEnhancement()).toBe(true); });
  await send('retranscription-complete', { meeting_id: 'meeting', warning: null });
  expect(ctx.run).toBeNull();
  expect(success).not.toHaveBeenCalled();
});

test('a rejected cancel leaves the run tracked', async () => {
  invoke.mockImplementationOnce(async () => { throw new Error('No retranscription in progress'); });
  await act(async () => ctx.markStarted('meeting'));
  await act(async () => { expect(await ctx.cancelEnhancement()).toBe(false); });
  expect(ctx.run?.status).toBe('running');
  await send('retranscription-error', { meeting_id: 'meeting', error: 'Model crashed' });
  expect(error).toHaveBeenCalledTimes(1);
});
