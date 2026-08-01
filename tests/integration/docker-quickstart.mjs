import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { runNodeScript, sleep } from './_helpers.mjs';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const composeFile = path.join(repoRoot, 'docker/docker-compose.yml');
const dockerEnv = { ...process.env, EIGER_TOKEN: 'integration-secret' };
const startedAt = Date.now();

try {
  await run('docker', ['compose', '-f', composeFile, 'up', '--build', '-d'], repoRoot, { env: dockerEnv });

  for (let attempt = 0; attempt < 120; attempt += 1) {
    try {
      const response = await fetch('http://127.0.0.1:3000/health');
      if (response.ok) break;
    } catch {}
    await sleep(1000);
  }

  const response = await fetch('http://127.0.0.1:3000/health');
  if (!response.ok) {
    throw new Error(`health check failed after docker quickstart: ${response.status}`);
  }

  await runNodeScript('puppeteer-smoke.mjs', {
    EIGER_HOST: '127.0.0.1:3000',
    EIGER_TOKEN: dockerEnv.EIGER_TOKEN,
  });

  console.log(JSON.stringify({ ok: true, elapsedSeconds: Math.round((Date.now() - startedAt) / 1000) }));
} finally {
  await run('docker', ['compose', '-f', composeFile, 'down', '--remove-orphans'], repoRoot, { allowFailure: true, env: dockerEnv });
}

async function run(command, args, cwd, { allowFailure = false, env = process.env } = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { cwd, stdio: 'inherit', env });
    child.once('exit', (code) => {
      if (code === 0 || allowFailure) resolve();
      else reject(new Error(`${command} ${args.join(' ')} exited ${code}`));
    });
    child.once('error', reject);
  });
}
