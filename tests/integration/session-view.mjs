import { withEigerServer } from './_server.mjs';

await withEigerServer({}, async ({ httpBaseUrl }) => {
  const session = await postJson(`${httpBaseUrl}/sessions`, {});

  try {
    const response = await fetch(`${httpBaseUrl}/sessions/view`);
    if (!response.ok) {
      throw new Error(`GET /sessions/view failed: ${response.status} ${await response.text()}`);
    }
    if (!response.headers.get('content-type')?.startsWith('text/html')) {
      throw new Error(`expected text/html, got ${response.headers.get('content-type')}`);
    }

    const html = await response.text();
    for (const expected of [
      '<table',
      session.id,
      String(session.pid),
      'devtools://devtools/bundled/inspector.html?ws=',
      '<th>rss</th>',
      '<th>cpu</th>',
    ]) {
      if (!html.includes(expected)) {
        throw new Error(`expected sessions view to include ${expected}`);
      }
    }

    console.log(JSON.stringify({ ok: true }));
  } finally {
    await fetch(`${httpBaseUrl}/sessions/${session.id}`, { method: 'DELETE' });
  }
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
