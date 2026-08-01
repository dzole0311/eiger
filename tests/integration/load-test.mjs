import { cdpRequest, fetchText, getSessions, metricValue, openCdpWebSocket, sleep, waitFor } from './_helpers.mjs';

const options = parseArgs();
const startedAt = Date.now();
const deadline = startedAt + options.duration * 1000;
const stats = {
  durationSeconds: options.duration,
  concurrency: options.concurrency,
  iterations: 0,
  failures: 0,
};

await Promise.all(
  Array.from({ length: options.concurrency }, (_, workerId) => worker(workerId)),
);

await waitFor(
  async () => (await getSessions()).length === 0,
  { timeoutMs: 30000, label: 'all load-test sessions to recycle' },
);

const metrics = await fetchText('/metrics');
stats.createdTotal = metricValue(metrics, 'eiger_sessions_created_total');
stats.rejectedTotal = metricValue(metrics, 'eiger_sessions_rejected_total');
stats.hardKilledTotal = metricValue(metrics, 'eiger_sessions_hard_killed_total');

if (stats.failures > 0) {
  throw new Error(`load test had ${stats.failures} failures: ${JSON.stringify(stats)}`);
}

console.log(JSON.stringify(stats));

async function worker(workerId) {
  while (Date.now() < deadline) {
    let ws;
    try {
      ws = await openCdpWebSocket('/session');
      await cdpRequest(ws, 'Target.getTargets');
      stats.iterations += 1;
      ws.close();
      await sleep(options.pauseMs);
    } catch (error) {
      stats.failures += 1;
      console.error(JSON.stringify({ workerId, error: error.message }));
      await sleep(500);
    } finally {
      if (ws && ws.readyState < 2) {
        ws.terminate();
      }
    }
  }
}

function parseArgs() {
  const args = new Map();
  for (let index = 2; index < process.argv.length; index += 1) {
    const [key, value] = process.argv[index].replace(/^--/, '').split('=');
    if (value === undefined) {
      args.set(key, process.argv[index + 1]);
      index += 1;
    } else {
      args.set(key, value);
    }
  }

  return {
    duration: Number(args.get('duration') ?? process.env.EIGER_LOAD_DURATION_SECONDS ?? 60),
    concurrency: Number(args.get('concurrency') ?? process.env.EIGER_LOAD_CONCURRENCY ?? 4),
    pauseMs: Number(args.get('pause-ms') ?? process.env.EIGER_LOAD_PAUSE_MS ?? 100),
  };
}
