import type { Page } from '@playwright/test';
import { readFile } from 'node:fs/promises';
import path from 'node:path';

const outputDirectory = path.resolve(__dirname, '../../out');
const contentTypes: Record<string, string> = {
  '.html': 'text/html', '.txt': 'text/plain', '.js': 'application/javascript',
  '.css': 'text/css', '.woff2': 'font/woff2', '.json': 'application/json',
};

export function collectAppErrors(page: Page) {
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  // React also sends caught render failures to console.error.
  page.on('console', message => {
    if (message.type() === 'error') errors.push(message.text());
  });
  page.on('dialog', async dialog => {
    errors.push(`${dialog.type()}: ${dialog.message()}`);
    await dialog.dismiss();
  });
  return errors;
}

export async function serveProductionApp(page: Page) {
  // Serve the same static export used by the app bundle, without a dev server.
  await page.route('https://meetily.test/**', async route => {
    const url = new URL(route.request().url());
    let filename = path.join(outputDirectory, decodeURIComponent(url.pathname));
    if (!path.extname(filename)) filename += url.pathname.endsWith('/') ? 'index.html' : '.html';
    await route.fulfill({
      body: await readFile(filename),
      contentType: contentTypes[path.extname(filename)] ?? 'application/octet-stream',
    });
  });
  // No network services are part of this fixture. Fail closed for other URLs.
  await page.route(url => url.origin !== 'https://meetily.test', route => route.abort());
}
