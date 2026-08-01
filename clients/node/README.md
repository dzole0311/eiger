# Eiger Node Client

Typed Node client for Eiger.

## Quickstart

```bash
npm install
npm run build
```

```ts
import puppeteer from 'puppeteer';
import { EigerClient } from '@eiger-browser/node';

const eiger = new EigerClient({
  baseUrl: 'http://127.0.0.1:3000',
});

const browserWSEndpoint = await eiger.connect();
const browser = await puppeteer.connect({ browserWSEndpoint });
const page = await browser.newPage();

await page.goto('https://example.com', { waitUntil: 'domcontentloaded' });
console.log(await page.title());
await browser.close();
```

Use the REST helpers when you do not need a full Puppeteer session:

```ts
const scrape = await eiger.scrape({
  url: 'https://example.com',
  waitUntil: 'domcontentloaded',
});

console.log(scrape.title);
```
