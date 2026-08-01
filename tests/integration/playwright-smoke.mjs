import { chromium } from 'playwright-core';
import { wsUrl } from './_helpers.mjs';

const endpoint = process.env.EIGER_WS_ENDPOINT ?? wsUrl('/session');

const browser = await chromium.connectOverCDP(endpoint);
const page = await browser.newPage();
await page.goto(process.env.EIGER_TEST_URL ?? 'https://example.com', {
  waitUntil: 'domcontentloaded',
});

const title = await page.title();
console.log(JSON.stringify({ endpoint, title }));

if (!title) {
  throw new Error('expected page title');
}

await browser.close();
