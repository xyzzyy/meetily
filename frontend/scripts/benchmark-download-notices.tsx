// Compare the real hooks with deterministic timers and a mocked desktop boundary.
// Usage: bun scripts/benchmark-download-notices.tsx --out=/tmp/notices.json --prior=967e35a
import React from 'react';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { mock } from 'bun:test';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { cpus } from 'node:os';
import path from 'node:path';

const root = path.resolve(import.meta.dir, '../..');
const frontend = path.join(root, 'frontend');
const temporary = mkdtempSync(path.join(frontend, '.notice-bench-'));
const options = Object.fromEntries(process.argv.slice(2).map(value => value.replace(/^--/, '').split('=')));
const handlers = new Map<string, (event: any) => unknown>();
let now = 0, nextId = 0, writes = 0, queries = 0, wakeups = 0, ready = false;
let completedAt: number | null = null, dismissedAt: number | null = null;
const timers = new Map<number, { at: number; interval: number; callback: () => unknown }>();
function schedule(callback: () => unknown, delay: number, interval = 0) {
  const id = ++nextId;
  timers.set(id, { at: now + delay, interval, callback });
  return id;
}
const realTimers = { setTimeout, clearTimeout, setInterval, clearInterval };
Object.assign(globalThis, {
  setTimeout: (callback: () => unknown, delay: number) => schedule(callback, delay),
  clearTimeout: (id: number) => timers.delete(id),
  setInterval: (callback: () => unknown, delay: number) => schedule(callback, delay, delay),
  clearInterval: (id: number) => timers.delete(id),
});
const core = { ...await import('@tauri-apps/api/core') };
const events = { ...await import('@tauri-apps/api/event') };
const sonner = { ...await import('sonner') };
mock.module('@tauri-apps/api/core', () => ({ ...core, invoke: async (command: string) => {
  queries++;
  if (command === 'get_ollama_download_status') return ready ? 'completed' : 'downloading';
  if (command === 'get_diarization_model_status') return { installed: ready, downloading: !ready };
  return ['parakeet', 'whisper', 'funasr', 'nemotron', 'summary'].map(name => ({
    name, status: ready ? (command === 'builtin_ai_list_models' ? { type: 'available' } : 'Available') : { Downloading: { progress: 99 } },
  }));
} }));
mock.module('@tauri-apps/api/event', () => ({ ...events, listen: async (name: string, callback: any) => {
  handlers.set(name, callback);
  return () => handlers.delete(name);
} }));
mock.module('sonner', () => ({ ...sonner, toast: {
  custom: (render: any) => {
    writes++;
    if (render('toast').props.download.status === 'completed' && completedAt === null) completedAt = now;
  },
  dismiss: () => { if (dismissedAt === null) dismissedAt = now; },
} }));
const sourcePath = 'frontend/src/components/shared/DownloadProgressToast.tsx';
const sourceAt = (ref: string, file = sourcePath) => execFileSync('git', ['show', `${ref}:${file}`], { cwd: root, encoding: 'utf8' });
const versions: { label: string; ref: string; source: string; Provider: React.ComponentType }[] = [];
try {
  for (const [label, ref] of [['main', options.base ?? 'origin/main'], ...(options.prior ? [['previous-pr', options.prior]] : [])]) {
    let source = sourceAt(ref);
    if (source.includes("@/lib/model-downloads")) {
      writeFileSync(path.join(temporary, `${label}-adapters.ts`), sourceAt(ref, 'frontend/src/lib/model-downloads.ts'));
      source = source.replace("@/lib/model-downloads", `./${label}-adapters`);
    }
    const file = path.join(temporary, `${label}.tsx`);
    writeFileSync(file, source);
    versions.push({ label, ref: execFileSync('git', ['rev-parse', ref], { cwd: root, encoding: 'utf8' }).trim(), source, Provider: (await import(file)).DownloadProgressToastProvider });
  }
  const source = readFileSync(path.join(root, sourcePath), 'utf8');
  versions.push({ label: 'working-tree', ref: createHash('sha256').update(source).digest('hex'), source,
    Provider: (await import('../src/components/shared/DownloadProgressToast')).DownloadProgressToastProvider });

  const progressEvents: [string, any][] = [
    ['parakeet-model-download-progress', { modelName: 'parakeet', progress: 40, downloaded_mb: 40, total_mb: 100, status: 'downloading' }],
    ['builtin-ai-download-progress', { model: 'summary', progress: 40, downloaded_mb: 40, total_mb: 100, status: 'downloading' }],
    ['model-download-progress', { modelName: 'whisper', progress: 40 }],
    ['funasr-model-download-progress', { modelName: 'funasr', progress: 40 }],
    ['nemotron-download-progress', { model: 'nemotron', percent: 40 }],
    ['ollama-model-download-progress', { modelName: 'ollama', progress: 40 }],
    ['diarization-model-download-progress', { percent: 40 }],
  ];
  async function mount(Provider: React.ComponentType) {
    handlers.clear(); timers.clear(); now = 0; writes = 0; queries = 0; wakeups = 0; ready = false;
    completedAt = null; dismissedAt = null;
    let renderer!: ReactTestRenderer;
    await act(async () => { renderer = create(<Provider />); });
    return renderer;
  }
  async function emit(name: string, payload: any) {
    await act(async () => { await handlers.get(name)?.({ payload }); });
  }
  async function advance(to: number) {
    for (;;) {
      const due = [...timers.entries()].filter(([, timer]) => timer.at <= to).sort((a, b) => a[1].at - b[1].at)[0];
      if (!due) break;
      const [id, timer] = due;
      now = timer.at;
      if (timer.interval) { timer.at += timer.interval; wakeups++; } else timers.delete(id);
      await act(async () => { await timer.callback(); });
    }
    now = to;
  }
  const quantile = (values: number[], q: number) => {
    const sorted = [...values].sort((a, b) => a - b), position = (sorted.length - 1) * q;
    return sorted[Math.floor(position)] + (sorted[Math.ceil(position)] - sorted[Math.floor(position)]) * (position % 1);
  };
  const results: any[] = [];
  for (const version of versions) {
    let renderer = await mount(version.Provider);
    const subscriptions = handlers.size;
    await advance(60000);
    const idle = { timer_wakeups_per_minute: wakeups, desktop_queries_per_minute: queries };
    await act(async () => renderer.unmount());
    renderer = await mount(version.Provider);
    await emit(progressEvents[0][0], { ...progressEvents[0][1], progress: 99 });
    await advance(5000); ready = true; await advance(35000);
    const recovery = { completed_ms_after_ready: completedAt === null ? null : completedAt - 5000,
      explicit_dismiss_ms_after_ready: dismissedAt === null ? null : dismissedAt - 5000,
      observed_ms_after_ready: 30000 };
    await act(async () => renderer.unmount());
    const latencies: number[] = [];
    for (let phase = 100; phase < 2000; phase += 200) {
      renderer = await mount(version.Provider);
      await emit(progressEvents[0][0], { ...progressEvents[0][1], progress: 99 });
      await advance(phase); ready = true; await advance(10000);
      if (completedAt !== null) latencies.push(completedAt - phase);
      await act(async () => renderer.unmount());
    }
    renderer = await mount(version.Provider);
    await emit(...progressEvents[0]); await advance(60000);
    const active = { desktop_queries_per_minute: queries, timer_wakeups_per_minute: wakeups };
    await act(async () => renderer.unmount());
    results.push({ version: version.label, ref_or_source_sha256: version.ref, subscriptions, idle, active, recovery,
      recovery_phase_samples_ms: latencies, recovery_p50_ms: latencies.length ? quantile(latencies, 0.5) : null,
      recovery_p95_ms: latencies.length ? quantile(latencies, 0.95) : null, event_batches: [] });
  }
  // Rotate order to reduce warm-up and machine-load bias. Discard the first round.
  for (let round = 0; round < 6; round++) for (let index = 0; index < versions.length; index++) {
    const version = versions[(index + round) % versions.length];
    for (const count of [2, 7]) {
      if (count === 7 && version.label === 'main') continue;
      const renderer = await mount(version.Provider);
      for (const event of progressEvents.slice(0, count)) await emit(...event);
      const initialWrites = writes, samples: number[] = [], cpu = process.cpuUsage(), started = performance.now();
      for (let event = 0; event < 1000; event++) {
        const start = performance.now();
        await emit(progressEvents[0][0], { ...progressEvents[0][1], progress: event % 98 + 1 });
        samples.push(performance.now() - start);
      }
      const elapsed = performance.now() - started, usage = process.cpuUsage(cpu);
      if (round) results.find(result => result.version === version.label).event_batches.push({ active_models: count, events: 1000,
        toast_updates: writes - initialWrites, elapsed_ms: elapsed, cpu_ms: (usage.user + usage.system) / 1000,
        p50_handler_ms: quantile(samples, 0.5), p95_handler_ms: quantile(samples, 0.95) });
      await act(async () => renderer.unmount());
    }
  }
  const report = { methodology: 'React 18 development test renderer; actual hooks; mocked Tauri IPC and toast sink; deterministic clock for recovery; five timed batches of 1000 events after warm-up. No DOM layout, Sonner rendering, real IPC, network transfer, or transcription benchmark.',
    environment: { bun: Bun.version, node_compat: process.version, platform: process.platform, arch: process.arch, cpu: cpus()[0]?.model }, results };
  const json = JSON.stringify(report, null, 2);
  if (options.out) writeFileSync(options.out, json+'\n'); else console.log(json);
} finally {
  Object.assign(globalThis, realTimers);
  mock.module('@tauri-apps/api/core', () => core); mock.module('@tauri-apps/api/event', () => events); mock.module('sonner', () => sonner);
  rmSync(temporary, { recursive: true, force: true });
}
