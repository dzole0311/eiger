import { httpUrl } from './_helpers.mjs';
import { withEigerServer } from './_server.mjs';

await withEigerServer({
  EIGER_CHROME_EXECUTABLE: '/missing/eiger-chrome',
}, async ({ httpBaseUrl }) => {
  const health = await fetch(`${httpBaseUrl}/health`);
  if (!health.ok) {
    throw new Error(`cold health check failed: ${health.status}`);
  }

  const ready = await fetch(`${httpBaseUrl}/ready`);
  const body = await ready.json();

  if (ready.status !== 503) {
    throw new Error(`cold server should not be ready before a launch, got ${ready.status}`);
  }

  if (body.canAcceptSessions || body.browserLaunchReady) {
    throw new Error(`cold readiness body was ready: ${JSON.stringify(body)}`);
  }
});

const health = await fetch(httpUrl('/health'));
if (!health.ok) {
  throw new Error(`health check failed: ${health.status}`);
}

const ready = await fetch(httpUrl('/ready'));
const body = await ready.json();

if (!ready.ok) {
  throw new Error(`ready check failed: ${ready.status} ${JSON.stringify(body)}`);
}

if (!body.canAcceptSessions || !body.browserLaunchReady || body.availableCapacity < 1) {
  throw new Error(`expected ready server with capacity, got ${JSON.stringify(body)}`);
}

console.log(JSON.stringify({ ok: true, readiness: body }));
