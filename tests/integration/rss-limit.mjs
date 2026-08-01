import puppeteer from 'puppeteer-core';
import { fetchJson, fetchText, getSessions, metricValue, waitFor } from './_helpers.mjs';

const leakMb = Number(process.env.EIGER_LEAK_MB ?? 384);
const timeoutMs = Number(process.env.EIGER_RSS_LIMIT_TIMEOUT_MS ?? 45000);
const beforeMetrics = await fetchText('/metrics');
const before = metricValue(beforeMetrics, 'eiger_sessions_rss_limit_recycled_total');
const session = await fetchJson('/sessions', { method: 'POST', body: '{}' });
let browser;

try {
  try {
    browser = await puppeteer.connect({ browserWSEndpoint: session.cdpWsUrl });
    const page = await browser.newPage();
    await page.goto('data:text/html,<title>eiger-rss-limit</title>');
    await page.evaluate((megabytes) => {
      window.__eigerLeak = [];
      for (let index = 0; index < megabytes; index += 1) {
        const buffer = new Uint8Array(1024 * 1024);
        buffer.fill(index % 255);
        window.__eigerLeak.push(buffer);
      }
      return window.__eigerLeak.length;
    }, leakMb);
  } catch (error) {
    console.error(JSON.stringify({ sessionId: session.id, pid: session.pid, leakSetupError: error.message }));
  }

  await waitFor(
    async () => {
      const sessions = await getSessions();
      return sessions.every((candidate) => candidate.id !== session.id);
    },
    { timeoutMs, intervalMs: 500, label: 'RSS limit session recycle' },
  );

  const afterMetrics = await fetchText('/metrics');
  const after = metricValue(afterMetrics, 'eiger_sessions_rss_limit_recycled_total');

  if (after <= before) {
    throw new Error(`expected RSS recycle counter to increase; before=${before} after=${after}`);
  }

  console.log(JSON.stringify({ ok: true, sessionId: session.id, pid: session.pid, leakMb, before, after }));
} finally {
  if (browser) {
    browser.disconnect();
  }

  const sessions = await getSessions();
  if (sessions.some((candidate) => candidate.id === session.id)) {
    await fetchJson(`/sessions/${session.id}`, { method: 'DELETE' });
  }
}
