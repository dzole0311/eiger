import { spawn } from 'node:child_process';
import net from 'node:net';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { sleep } from './_helpers.mjs';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');

export async function withEigerServer(extraEnv, callback) {
  const port = await freePort();
  const host = `127.0.0.1:${port}`;
  const httpBaseUrl = `http://${host}`;
  const child = spawn('cargo', ['run', '-p', 'eiger-server'], {
    cwd: repoRoot,
    stdio: ['ignore', 'pipe', 'pipe'],
    env: {
      ...process.env,
      EIGER_BIND_ADDR: host,
      ...extraEnv,
    },
  });
  let output = '';

  child.stdout.on('data', (chunk) => {
    output += chunk.toString();
  });
  child.stderr.on('data', (chunk) => {
    output += chunk.toString();
  });

  try {
    await waitForServer(child, httpBaseUrl, () => output);
    return await callback({ host, httpBaseUrl });
  } finally {
    await stopServer(child);
  }
}

async function waitForServer(child, httpBaseUrl, output) {
  const startedAt = Date.now();
  while (Date.now() - startedAt < 30000) {
    if (child.exitCode !== null) {
      throw new Error(`eiger server exited ${child.exitCode}\n${output()}`);
    }

    try {
      const response = await fetch(`${httpBaseUrl}/health`);
      if (response.ok) {
        return;
      }
    } catch {}

    await sleep(250);
  }

  throw new Error(`timed out waiting for eiger server\n${output()}`);
}

async function stopServer(child) {
  if (child.exitCode !== null) {
    return;
  }

  child.kill('SIGINT');

  const exited = await new Promise((resolve) => {
    const timer = setTimeout(() => resolve(false), 5000);
    child.once('exit', () => {
      clearTimeout(timer);
      resolve(true);
    });
  });

  if (!exited) {
    child.kill('SIGKILL');
  }
}

async function freePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.unref();
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const address = server.address();
      server.close(() => resolve(address.port));
    });
  });
}
