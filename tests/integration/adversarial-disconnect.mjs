import { cdpRequest, getSessions, openCdpWebSocket, waitFor } from './_helpers.mjs';

const ws = await openCdpWebSocket('/session');
await cdpRequest(ws, 'Target.getTargets');

const [session] = await waitFor(
  async () => {
    const sessions = await getSessions();
    return sessions.length > 0 ? sessions : false;
  },
  { timeoutMs: 10000, label: 'session to become visible' },
);

await cdpRequest(ws, 'Target.createTarget', {
  url: 'data:text/html,<title>eiger-adversarial-disconnect</title>',
});

ws.terminate();

await waitFor(
  async () => {
    const sessions = await getSessions();
    return sessions.every((candidate) => candidate.id !== session.id);
  },
  { timeoutMs: 15000, label: 'session cleanup after forceful client disconnect' },
);

console.log(JSON.stringify({ ok: true, sessionId: session.id, pid: session.pid }));
