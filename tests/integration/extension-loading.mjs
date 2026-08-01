import { execFile } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import puppeteer from 'puppeteer-core';
import { waitFor } from './_helpers.mjs';
import { withEigerServer } from './_server.mjs';

const execFileAsync = promisify(execFile);
const testDir = path.dirname(fileURLToPath(import.meta.url));
const extensionPath = path.join(testDir, 'fixtures', 'marker-extension');

await withEigerServer({}, async ({ httpBaseUrl }) => {
  const session = await postJson(`${httpBaseUrl}/sessions`, {
    stealthEnabled: false,
    extensionPaths: [extensionPath],
  });

  const command = await browserCommand(session.pid);
  if (commandLineBlocksExtensions(command)) {
    await deleteSession(httpBaseUrl, session.id);
    console.log(JSON.stringify({
      ok: true,
      skipped: 'official Chrome builds block command-line extension loading',
    }));
    return;
  }

  const browser = await puppeteer.connect({ browserWSEndpoint: session.cdpWsUrl });
  try {
    const marker = await waitFor(async () => {
      for (const target of browser.targets().filter((target) => target.type() === 'service_worker')) {
        const worker = await target.worker();
        const marker = worker && await worker.evaluate(() => globalThis.EIGER_EXTENSION_MARKER);
        if (marker === 'loaded') {
          return marker;
        }
      }
      return null;
    }, { timeoutMs: 10000, intervalMs: 100, label: 'extension worker marker' });
    if (marker !== 'loaded') {
      throw new Error(`expected extension worker marker, got ${marker}`);
    }
  } finally {
    await browser.close();
  }

  console.log(JSON.stringify({ ok: true }));
});

async function postJson(url, body) {
  const response = await fetch(url, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });

  if (!response.ok) {
    throw new Error(`POST ${url} failed: ${response.status} ${await response.text()}`);
  }

  return response.json();
}

async function deleteSession(httpBaseUrl, id) {
  const response = await fetch(`${httpBaseUrl}/sessions/${id}`, { method: 'DELETE' });
  if (response.status !== 204) {
    throw new Error(`DELETE /sessions/${id} failed: ${response.status} ${await response.text()}`);
  }
}

async function browserCommand(pid) {
  if (process.platform === 'win32') {
    return '';
  }

  const { stdout } = await execFileAsync('ps', ['-p', String(pid), '-o', 'command=']);
  return stdout.trim();
}

function commandLineBlocksExtensions(command) {
  const normalized = command.toLowerCase();
  if (normalized.includes('chrome for testing') || normalized.includes('chrome-for-testing')) {
    return false;
  }
  return normalized.includes('google chrome.app') || normalized.includes('google-chrome');
}
