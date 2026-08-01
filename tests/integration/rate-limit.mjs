import { withEigerServer } from './_server.mjs';
import { sleep } from './_helpers.mjs';

await withEigerServer({
  EIGER_TOKEN: 'secret',
  EIGER_RATE_LIMIT_RPS: '1',
  EIGER_RATE_LIMIT_BURST: '1',
  EIGER_CHROME_EXECUTABLE: '/missing/eiger-chrome',
}, async ({ httpBaseUrl }) => {
  await sleep(1100);

  const request = () => fetch(`${httpBaseUrl}/sessions`, {
    method: 'POST',
    headers: {
      authorization: 'Bearer secret',
      'content-type': 'application/json',
    },
    body: '{}',
  });

  const first = await request();
  if (first.status === 429) {
    throw new Error(`first request was rate limited: ${await first.text()}`);
  }

  const second = await request();
  const body = await second.json();

  if (second.status !== 429) {
    throw new Error(`expected 429, got ${second.status}: ${JSON.stringify(body)}`);
  }

  if (!body.error?.includes('rate limit exceeded')) {
    throw new Error(`expected clear rate limit body, got ${JSON.stringify(body)}`);
  }

  console.log(JSON.stringify({
    ok: true,
    firstStatus: first.status,
    secondStatus: second.status,
    retryAfter: second.headers.get('retry-after'),
  }));
});
