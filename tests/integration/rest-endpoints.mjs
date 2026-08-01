import { withEigerServer } from './_server.mjs';
import { waitFor } from './_helpers.mjs';

await withEigerServer({}, async ({ httpBaseUrl }) => {
  const url = dataUrl(`
    <!doctype html>
    <title>Eiger REST</title>
    <style>
      body { margin: 0; background: #f8fafc; color: #111827; font-family: system-ui, sans-serif; }
      main { width: 720px; min-height: 1400px; padding: 48px; background: #ffffff; }
      h1 { color: #0f766e; }
    </style>
    <main>
      <h1 id="marker">REST endpoints work</h1>
      <p>Rendered by Chrome through Eiger.</p>
    </main>
  `);

  const scrape = await postJson(httpBaseUrl, '/scrape', {
    url,
    waitUntil: 'domcontentloaded',
    timeoutMs: 10000,
  });
  const scrapeBody = await scrape.json();
  if (scrapeBody.title !== 'Eiger REST') {
    throw new Error(`expected scraped title, got ${JSON.stringify(scrapeBody)}`);
  }
  if (!scrapeBody.html.includes('REST endpoints work')) {
    throw new Error('expected scraped HTML to include marker text');
  }

  const screenshot = await postJson(httpBaseUrl, '/screenshot', {
    url,
    waitUntil: 'load',
    timeoutMs: 10000,
    fullPage: true,
    format: 'png',
  });
  if (!screenshot.headers.get('content-type')?.startsWith('image/png')) {
    throw new Error(`expected image/png, got ${screenshot.headers.get('content-type')}`);
  }
  assertMagic(await screenshot.arrayBuffer(), [0x89, 0x50, 0x4e, 0x47], 'PNG');

  const pdf = await postJson(httpBaseUrl, '/pdf', {
    url,
    waitUntil: 'load',
    timeoutMs: 10000,
    format: 'Letter',
    printBackground: true,
  });
  if (!pdf.headers.get('content-type')?.startsWith('application/pdf')) {
    throw new Error(`expected application/pdf, got ${pdf.headers.get('content-type')}`);
  }
  assertMagic(await pdf.arrayBuffer(), [0x25, 0x50, 0x44, 0x46], 'PDF');

  await waitFor(async () => {
    const response = await fetch(`${httpBaseUrl}/sessions`);
    const sessions = await response.json();
    return sessions.length === 0;
  }, { label: 'REST endpoint session cleanup' });

  console.log(JSON.stringify({ ok: true }));
});

async function postJson(httpBaseUrl, path, body) {
  const response = await fetch(`${httpBaseUrl}${path}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });

  if (!response.ok) {
    throw new Error(`POST ${path} failed: ${response.status} ${await response.text()}`);
  }

  return response;
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
