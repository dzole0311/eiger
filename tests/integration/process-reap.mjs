import { fetchJson, getSessions, pidExists, waitFor } from './_helpers.mjs';

const session = await fetchJson('/sessions', { method: 'POST', body: '{}' });

await fetchJson(`/sessions/${session.id}`, { method: 'DELETE' });

await waitFor(
  async () => !(await pidExists(session.pid)),
  { timeoutMs: 10000, label: `pid ${session.pid} to be absent` },
);

await waitFor(
  async () => (await getSessions()).every((candidate) => candidate.id !== session.id),
  { timeoutMs: 10000, label: 'deleted session to leave pool listing' },
);

console.log(JSON.stringify({ ok: true, sessionId: session.id, pid: session.pid }));
