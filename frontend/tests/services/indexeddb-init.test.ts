import { afterEach, expect, test } from 'bun:test';

const originalIndexedDB = Object.getOwnPropertyDescriptor(globalThis, 'indexedDB');
afterEach(() => {
  if (originalIndexedDB) Object.defineProperty(globalThis, 'indexedDB', originalIndexedDB);
  else Reflect.deleteProperty(globalThis, 'indexedDB');
});

async function service() {
  return (await import(`../../src/services/indexedDBService.ts?test=${Math.random()}`)).indexedDBService;
}

test('concurrent callers share a failed open and a later call can retry', async () => {
  const requests: any[] = [];
  Object.defineProperty(globalThis, 'indexedDB', { configurable: true, value: {
    open() { const request = {}; requests.push(request); return request; },
  } });
  const db = await service();
  const first = db.init();
  const second = db.init();
  const failed = Promise.allSettled([first, second]);
  expect(requests).toHaveLength(1);
  requests[0].error = new Error('temporary failure');
  requests[0].onerror();
  expect((await failed).map(r => r.status)).toEqual(['rejected', 'rejected']);

  const retry = db.init();
  const concurrentRetry = db.init();
  expect(requests).toHaveLength(2);
  requests[1].result = {};
  requests[1].onsuccess();
  await Promise.all([retry, concurrentRetry]);
  await db.init();
  expect(requests).toHaveLength(2);
});

test('a synchronous open error does not prevent a later attempt', async () => {
  let attempts = 0;
  Object.defineProperty(globalThis, 'indexedDB', { configurable: true, value: {
    open() {
      attempts++;
      throw new Error('storage unavailable');
    },
  } });
  const db = await service();
  await expect(db.init()).rejects.toThrow('storage unavailable');
  await expect(db.init()).rejects.toThrow('storage unavailable');
  expect(attempts).toBe(2);
});
