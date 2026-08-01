import { withEigerServer } from './_server.mjs';

await withEigerServer({
  EIGER_REQUEST_BODY_LIMIT_BYTES: '16',
  EIGER_CHROME_EXECUTABLE: '/missing/eiger-chrome',
}, async ({ httpBaseUrl }) => {
  const response = await fetch(`${httpBaseUrl}/sessions`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ extraChromeArgs: ['--window-size=1280,720'] }),
  });

  if (response.status !== 413) {
    throw new Error(`expected 413 for oversized body, got ${response.status}: ${await response.text()}`);
  }

  console.log(JSON.stringify({ ok: true, status: response.status }));
});
