import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const composeFiles = [
  'docker/docker-compose.yml',
  'docker/docker-compose.low-shm.yml',
];

for (const file of composeFiles) {
  const contents = await readFile(path.join(repoRoot, file), 'utf8');

  if (contents.includes('change-me')) {
    throw new Error(`${file} still contains the sample token`);
  }

  if (!contents.includes('EIGER_TOKEN: "${EIGER_TOKEN:?')) {
    throw new Error(`${file} must require EIGER_TOKEN from .env or host env`);
  }
}

console.log(JSON.stringify({ ok: true, composeFiles }));
