import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import WebSocket from 'ws';

const execFileAsync = promisify(execFile);

export const host = process.env.EIGER_HOST ?? '127.0.0.1:3000';
export const httpBaseUrl = process.env.EIGER_HTTP_URL ?? `http://${host}`;
export const token = process.env.EIGER_TOKEN;

export function httpUrl(path, params = {}) {
  return withQuery(new URL(path, httpBaseUrl), params).toString();
}

export function wsUrl(path = '/session', params = {}) {
  const scheme = httpBaseUrl.startsWith('https:') ? 'wss:' : 'ws:';
  const url = new URL(path, httpBaseUrl);
  url.protocol = scheme;
  return withQuery(url, params).toString();
}

export async function fetchJson(path, options = {}) {
  const response = await fetch(httpUrl(path), {
    ...options,
    headers: {
      'content-type': 'application/json',
      ...(options.headers ?? {}),
    },
  });

  if (!response.ok) {
    throw new Error(`${options.method ?? 'GET'} ${path} failed: ${response.status} ${await response.text()}`);
  }

  if (response.status === 204) {
    return null;
  }

  return response.json();
}

export async function fetchText(path, options = {}) {
  const response = await fetch(httpUrl(path), options);
  if (!response.ok) {
    throw new Error(`${options.method ?? 'GET'} ${path} failed: ${response.status} ${await response.text()}`);
  }
  return response.text();
}

export async function getSessions() {
  return fetchJson('/sessions');
}

export function metricValue(metricsText, name) {
  const match = metricsText.match(new RegExp(`^${name}\\s+([0-9.]+)$`, 'm'));
  return match ? Number(match[1]) : 0;
}

export function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export async function waitFor(predicate, {
  timeoutMs = 10000,
  intervalMs = 250,
  label = 'condition',
} = {}) {
  const startedAt = Date.now();
  let lastValue;

  while (Date.now() - startedAt < timeoutMs) {
    lastValue = await predicate();
    if (lastValue) {
      return lastValue;
    }
    await sleep(intervalMs);
  }

  throw new Error(`timed out waiting for ${label}; last value: ${JSON.stringify(lastValue)}`);
}

export async function openCdpWebSocket(path = '/session', params = {}) {
  const ws = new WebSocket(wsUrl(path, params));
  await new Promise((resolve, reject) => {
    ws.once('open', resolve);
    ws.once('error', reject);
  });
  return ws;
}

export function cdpRequest(ws, method, params = {}, timeoutMs = 5000) {
  const id = nextCdpId++;

  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      ws.off('message', onMessage);
      reject(new Error(`CDP ${method} timed out`));
    }, timeoutMs);

    function onMessage(data) {
      const message = JSON.parse(data.toString());
      if (message.id !== id) {
        return;
      }

      clearTimeout(timeout);
      ws.off('message', onMessage);

      if (message.error) {
        reject(new Error(`CDP ${method} failed: ${JSON.stringify(message.error)}`));
      } else {
        resolve(message.result ?? null);
      }
    }

    ws.on('message', onMessage);
    ws.send(JSON.stringify({ id, method, params }));
  });
}

export async function pidExists(pid) {
  if (process.platform === 'win32') {
    return false;
  }

  try {
    const { stdout } = await execFileAsync('ps', ['-p', String(pid), '-o', 'pid=']);
    return stdout.trim().length > 0;
  } catch {
    return false;
  }
}

export async function runNodeScript(script, extraEnv = {}) {
  const { spawn } = await import('node:child_process');

  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [script], {
      stdio: 'inherit',
      env: { ...process.env, ...extraEnv },
    });
    child.once('exit', (code) => {
      if (code === 0) {
        resolve();
      } else {
        reject(new Error(`${script} exited ${code}`));
      }
    });
    child.once('error', reject);
  });
}

function withQuery(url, params) {
  if (token && !url.searchParams.has('token')) {
    url.searchParams.set('token', token);
  }

  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined && value !== null) {
      url.searchParams.set(key, String(value));
    }
  }

  return url;
}

let nextCdpId = 1;
