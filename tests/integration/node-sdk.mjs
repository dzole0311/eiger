import puppeteer from 'puppeteer-core';
import { EigerClient } from '../../clients/node/dist/index.js';
import { waitFor } from './_helpers.mjs';
import { withEigerServer } from './_server.mjs';

await withEigerServer({}, async ({ httpBaseUrl }) => {
  const client = new EigerClient({ baseUrl: httpBaseUrl });
  const url = dataUrl('<!doctype html><title>SDK Test</title><h1>node sdk</h1>');

  const browserWSEndpoint = await client.connect({ stealthEnabled: false });
  const browser = await puppeteer.connect({ browserWSEndpoint });
  try {
    const page = await browser.newPage();
    await page.goto(url, { waitUntil: 'domcontentloaded' });
    const title = await page.title();
    if (title !== 'SDK Test') {
      throw new Error(`expected SDK Test title, got ${title}`);
    }
  } finally {
    await browser.close();
  }
  await waitForNoSessions(client);

  const session = await client.createSession({ stealthEnabled: false });
  const fetched = await client.getSession(session.id);
  if (fetched.id !== session.id) {
    throw new Error(`expected fetched session ${session.id}, got ${fetched.id}`);
  }
  const sessions = await client.listSessions();
  if (!sessions.some((listed) => listed.id === session.id)) {
    throw new Error(`expected listSessions to include ${session.id}`);
  }
  if (!client.sessionWebSocketUrl(session.id).includes(`/sessions/${session.id}/cdp`)) {
    throw new Error('expected sessionWebSocketUrl to point at the session CDP route');
  }
  await client.deleteSession(session.id);

  const scrape = await client.scrape({ url, waitUntil: 'domcontentloaded' });
  if (scrape.title !== 'SDK Test' || !scrape.html.includes('node sdk')) {
    throw new Error(`unexpected scrape result ${JSON.stringify(scrape)}`);
  }

  const screenshot = await client.screenshot({
    url,
    waitUntil: 'load',
    format: 'png',
  });
  if (!screenshot.contentType.startsWith('image/png')) {
    throw new Error(`expected image/png, got ${screenshot.contentType}`);
  }
  assertMagic(screenshot.data, [0x89, 0x50, 0x4e, 0x47], 'PNG');

  const pdf = await client.pdf({
    url,
    waitUntil: 'load',
    format: 'Letter',
    printBackground: true,
  });
  if (!pdf.contentType.startsWith('application/pdf')) {
    throw new Error(`expected application/pdf, got ${pdf.contentType}`);
  }
  assertMagic(pdf.data, [0x25, 0x50, 0x44, 0x46], 'PDF');

  console.log(JSON.stringify({ ok: true }));
});

async function waitForNoSessions(client) {
  await waitFor(async () => {
    const sessions = await client.listSessions();
    return sessions.length === 0;
  }, { label: 'SDK session cleanup' });
}

function dataUrl(html) {
  return `data:text/html;charset=utf-8,${encodeURIComponent(html)}`;
}

function assertMagic(arrayBuffer, expected, label) {
  const bytes = new Uint8Array(arrayBuffer);
  for (const [index, byte] of expected.entries()) {
    if (bytes[index] !== byte) {
      throw new Error(`expected ${label} byte ${index} to be ${byte}, got ${bytes[index]}`);
    }
  }
}
