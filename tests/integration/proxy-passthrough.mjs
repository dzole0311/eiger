import { withEigerServer } from './_server.mjs';

const proxy = 'http://127.0.0.1:9';

await withEigerServer({}, async ({ httpBaseUrl }) => {
  const querySession = await createSession(`${httpBaseUrl}/sessions?proxy=${encodeURIComponent(proxy)}`);
  await deleteSession(httpBaseUrl, querySession.id);

  const bodySession = await createSession(`${httpBaseUrl}/sessions`, { proxy });
  await deleteSession(httpBaseUrl, bodySession.id);

  const bodyScrape = await postJson(`${httpBaseUrl}/scrape`, {
    url: dataUrl('<title>Proxy Body</title><h1>proxy body</h1>'),
    waitUntil: 'domcontentloaded',
    timeoutMs: 10000,
    proxy,
  });
  if (bodyScrape.title !== 'Proxy Body') {
    throw new Error(`expected body proxy scrape title, got ${JSON.stringify(bodyScrape)}`);
  }

  const queryScrape = await postJson(`${httpBaseUrl}/scrape?proxy=${encodeURIComponent(proxy)}`, {
    url: dataUrl('<title>Proxy Query</title><h1>proxy query</h1>'),
    waitUntil: 'domcontentloaded',
    timeoutMs: 10000,
  });
  if (queryScrape.title !== 'Proxy Query') {
    throw new Error(`expected query proxy scrape title, got ${JSON.stringify(queryScrape)}`);
  }

  console.log(JSON.stringify({ ok: true }));
});

async function createSession(url, body) {
  return postJson(url, body ?? {});
}

async function deleteSession(httpBaseUrl, id) {
  const response = await fetch(`${httpBaseUrl}/sessions/${id}`, { method: 'DELETE' });
  if (response.status !== 204) {
    throw new Error(`DELETE /sessions/${id} failed: ${response.status} ${await response.text()}`);
  }
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

function dataUrl(html) {
  return `data:text/html;charset=utf-8,${encodeURIComponent(html)}`;
}
