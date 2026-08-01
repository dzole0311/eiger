import fs from 'node:fs/promises';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import puppeteer from 'puppeteer-core';
import { pidExists, waitFor } from './_helpers.mjs';
import { withEigerServer } from './_server.mjs';

const profileStorageDir = await fs.mkdtemp(path.join(os.tmpdir(), 'eiger-profiles-'));

try {
  await withPageServer(async (pageUrl) => {
    await withEigerServer({
      EIGER_PROFILE_STORAGE_DIR: profileStorageDir,
    }, async ({ httpBaseUrl }) => {
      const firstSession = await withPersistentSession(httpBaseUrl, 'cookie-profile', async (browser) => {
        const page = await browser.newPage();
        await page.goto(pageUrl, { waitUntil: 'domcontentloaded' });
        await page.setCookie({
          name: 'eiger_profile_cookie',
          value: 'persisted',
          url: pageUrl,
          expires: Math.floor(Date.now() / 1000) + 3600,
        });
      });

      await waitForNoSessions(httpBaseUrl);
      await waitFor(
        async () => !(await pidExists(firstSession.pid)),
        { label: 'first profile browser exit' },
      );

      await withPersistentSession(httpBaseUrl, 'cookie-profile', async (browser) => {
        const page = await browser.newPage();
        await page.goto(pageUrl, { waitUntil: 'domcontentloaded' });
        const cookies = await page.cookies(pageUrl);
        const cookie = cookies.find((cookie) => cookie.name === 'eiger_profile_cookie');
        if (cookie?.value !== 'persisted') {
          throw new Error(`expected persistent cookie, got ${JSON.stringify(cookies)}`);
        }
      });

      console.log(JSON.stringify({ ok: true }));
    });
  });
} finally {
  await fs.rm(profileStorageDir, { recursive: true, force: true });
}

async function withPersistentSession(httpBaseUrl, persistentProfileId, callback) {
  const session = await postJson(`${httpBaseUrl}/sessions`, {
    persistentProfileId,
  });
  const browser = await puppeteer.connect({ browserWSEndpoint: session.cdpWsUrl });

  try {
    await callback(browser);
  } finally {
    await browser.close();
  }

  return session;
}

async function waitForNoSessions(httpBaseUrl) {
  await waitFor(async () => {
    const response = await fetch(`${httpBaseUrl}/sessions`);
    const sessions = await response.json();
    return sessions.length === 0;
  }, { label: 'profile session cleanup' });
}

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

async function withPageServer(callback) {
  const server = http.createServer((_, response) => {
    response.writeHead(200, { 'content-type': 'text/html' });
    response.end('<!doctype html><title>Profile Test</title><h1>profile test</h1>');
  });

  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });

  try {
    const { port } = server.address();
    await callback(`http://127.0.0.1:${port}/`);
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
}
