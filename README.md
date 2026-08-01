# Eiger

Eiger is a self-hosted headless-browser automation API. It gives Puppeteer and Playwright a stable Chrome DevTools Protocol endpoint with Rust process owning browser lifecycle, resource limits, health checks, etc.

## Quickstart

```bash
docker compose -f docker/docker-compose.yml up --build
```

Then connect Puppeteer:

```js
import puppeteer from 'puppeteer';

const browser = await puppeteer.connect({
  browserWSEndpoint: 'ws://127.0.0.1:3000/session',
});

const page = await browser.newPage();
await page.goto('https://example.com', { waitUntil: 'domcontentloaded' });
console.log(await page.title());
await browser.close();
```

The Compose file sets `shm_size: "1gb"` on purpose. A small `/dev/shm` is a common cause of Chromium crashes under concurrency in Docker.

## Local Development

Set `EIGER_CHROME_EXECUTABLE` if Chrome or Chromium is not discoverable on your `PATH`.

```bash
cargo run -p eiger-server
curl http://127.0.0.1:3000/health
```

Useful environment variables:

| Variable | Default | Purpose |
|---|---:|---|
| `EIGER_BIND_ADDR` | `0.0.0.0:3000` | HTTP/WebSocket bind address |
| `EIGER_TOKEN` | unset | Optional shared bearer/query token |
| `EIGER_CHROME_EXECUTABLE` | auto-detect | Chrome/Chromium executable path |
| `EIGER_CHROME_NO_SANDBOX` | `true` | Adds `--no-sandbox`, pragmatic for containers |
| `EIGER_CHROME_ARGS` | unset | Extra whitespace-separated Chrome flags |
| `EIGER_MAX_CONCURRENT_SESSIONS` | `4` | Global concurrency bound |
| `EIGER_PER_SESSION_RSS_LIMIT_MB` | `1536` | Soft RSS ceiling for the browser process tree |
| `EIGER_MAX_SESSION_LIFETIME_SECS` | `1800` | Max lifetime before recycle |
| `EIGER_MAX_IDLE_TIME_SECS` | `300` | Max idle time before recycle |
| `EIGER_STEALTH_ENABLED` | `true` | Baseline stealth flags and script injection |

## API

`GET /health` returns process health.

`GET /session` or `GET /` upgrades to a WebSocket, allocates a fresh Chromium session, proxies CDP to it, and recycles it when the WebSocket disconnects.

`POST /sessions` pre-warms a browser session and returns an Eiger CDP WebSocket URL:

```bash
curl -X POST 'http://127.0.0.1:3000/sessions'
```

`GET /sessions` lists active sessions with state, age, RSS, and CPU.

`GET /sessions/:id`, `DELETE /sessions/:id`, and `GET /sessions/:id/cdp` inspect, delete, and connect to a pre-warmed session.

`GET /metrics` returns Prometheus exposition format counters and per-session resource gauges.

## Baseline Stealth

Eiger includes baseline evasion and not an anti-bot arms race. It removes the default webdriver marker, strips `HeadlessChrome` from the user agent, sets a sane window size and injects basic navigator/WebGL patches into existing page targets. It does not attempt TLS fingerprint spoofing, CAPTCHA solving, mouse humanization or proxy rotation.

Disable it globally:

```bash
EIGER_STEALTH_ENABLED=false cargo run -p eiger-server
```

Disable it per direct WebSocket session:

```text
ws://127.0.0.1:3000/session?stealth=false
```

## Verification

With Eiger running:

```bash
cd tests/integration
npm install
npm run suite
```

See [docs/verification.md](docs/verification.md) for the full suite.

