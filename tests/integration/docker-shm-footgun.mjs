import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { runNodeScript, sleep } from './_helpers.mjs';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const composeFile = path.join(repoRoot, 'docker/docker-compose.low-shm.yml');

try {
  await run('docker', ['compose', '-f', composeFile, 'up', '--build', '-d'], repoRoot);

  for (let attempt = 0; attempt < 120; attempt += 1) {
    try {
      const response = await fetch('http://127.0.0.1:3000/health');
      if (response.ok) break;
    } catch {}
    await sleep(1000);
  }

  let failedAsExpected = false;
  try {
    await runNodeScript('load-test.mjs', {
      EIGER_HOST: '127.0.0.1:3000',
      EIGER_TOKEN: 'change-me',
      EIGER_LOAD_DURATION_SECONDS: '60',
      EIGER_LOAD_CONCURRENCY: '8',
    });
  } catch (error) {
    failedAsExpected = true;
    console.log(JSON.stringify({ ok: true, footgunConfirmed: true, error: error.message }));
  }

  if (!failedAsExpected) {
    throw new Error('low /dev/shm compose test did not fail; increase load or verify Docker default shm behavior on this host');
  }
} finally {
  await run('docker', ['compose', '-f', composeFile, 'down', '--remove-orphans'], repoRoot, { allowFailure: true });
}

async function run(command, args, cwd, { allowFailure = false } = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { cwd, stdio: 'inherit' });
    child.once('exit', (code) => {
      if (code === 0 || allowFailure) resolve();
      else reject(new Error(`${command} ${args.join(' ')} exited ${code}`));
    });
    child.once('error', reject);
  });
}
