import { withEigerServer } from './_server.mjs';

await withEigerServer({
  EIGER_CHROME_NO_SANDBOX: 'false',
  EIGER_MAX_CONCURRENT_SESSIONS: '1',
  EIGER_LAUNCH_QUEUE_TIMEOUT_SECS: '1',
  EIGER_RATE_LIMIT_RPS: '50',
  EIGER_RATE_LIMIT_BURST: '50',
}, async ({ httpBaseUrl }) => {
  let session;

  try {
    const first = await fetch(`${httpBaseUrl}/sessions`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: '{}',
    });
    session = await first.json();

    if (first.status !== 201) {
      throw new Error(`first session failed: ${first.status} ${JSON.stringify(session)}`);
    }

    const startedAt = Date.now();
    const second = await fetch(`${httpBaseUrl}/sessions`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: '{}',
    });
    const body = await second.json();
    const elapsedMs = Date.now() - startedAt;

    if (second.status !== 503) {
      throw new Error(`expected launch queue timeout 503, got ${second.status}: ${JSON.stringify(body)}`);
    }

    if (!body.error?.includes('timed out waiting for browser capacity')) {
      throw new Error(`expected capacity timeout body, got ${JSON.stringify(body)}`);
    }

    if (elapsedMs > 5000) {
      throw new Error(`queue timeout took too long: ${elapsedMs}ms`);
    }

    const ready = await fetch(`${httpBaseUrl}/ready`);
    const readiness = await ready.json();
    if (ready.status !== 503 || readiness.canAcceptSessions || readiness.availableCapacity !== 0) {
      throw new Error(`expected not ready at capacity, got ${ready.status} ${JSON.stringify(readiness)}`);
    }

    console.log(JSON.stringify({
      ok: true,
      sessionId: session.id,
      elapsedMs,
      readiness,
    }));
  } finally {
    if (session?.id) {
      await fetch(`${httpBaseUrl}/sessions/${session.id}`, { method: 'DELETE' });
    }
  }
});
