<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/eiger-logo-readme-dark.png">
    <source media="(prefers-color-scheme: light)" srcset="assets/eiger-logo-readme-light.png">
    <img src="assets/eiger-logo-readme-light.png" alt="Eiger" width="160">
  </picture>
</p>

# Eiger

Eiger is a self-hosted headless-browser automation API. It gives Puppeteer and Playwright a stable Chrome DevTools Protocol endpoint with Rust process owning browser lifecycle, resource limits, health checks, etc.

The name comes from the [Eiger](https://en.wikipedia.org/wiki/Eiger), a mountain in the Swiss Alps whose north face is famous as one of the hardest technical climbs in the Alps.

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

## Deployment

Eiger does not terminate TLS. If Eiger is reachable outside a trusted network, put it behind a TLS-terminating reverse proxy. Use nginx, Caddy or Traefik.

Eiger trusts `X-Forwarded-Proto` from that proxy when it builds returned CDP WebSocket URLs. Strip untrusted client-supplied forwarding headers at the proxy.

## Local Development

Set `EIGER_CHROME_EXECUTABLE` if Chrome or Chromium is not discoverable on your `PATH`.

```bash
cargo run -p eiger-server
curl http://127.0.0.1:3000/health
```

Useful environment variables:

| Variable | Default | Purpose |
|---|---:|---|
| `EIGER_BIND_ADDR` | `0.0.0.0:3000` | HTTP/WebSocket bind address. Use a TLS proxy for public access |
| `EIGER_TOKEN` | unset | Optional shared bearer/query token. Required by Compose examples |
| `EIGER_CORS_ORIGINS` | unset | Comma-separated exact origins allowed by CORS |
| `EIGER_REQUEST_BODY_LIMIT_BYTES` | `65536` | Max request body size for all routes |
| `EIGER_RATE_LIMIT_RPS` | `10` | Per-IP and per-token request refill rate |
| `EIGER_RATE_LIMIT_BURST` | `20` | Per-IP and per-token request burst size |
| `EIGER_CHROME_EXECUTABLE` | auto-detect | Chrome/Chromium executable path. `/ready` stays false until launch succeeds |
| `EIGER_CHROME_NO_SANDBOX` | `true` | Adds `--no-sandbox`, pragmatic for containers |
| `EIGER_CHROME_ARGS` | unset | Extra whitespace-separated Chrome flags |
| `EIGER_PROFILE_STORAGE_DIR` | unset | Base directory for opt-in persistent profiles |
| `EIGER_MAX_CONCURRENT_SESSIONS` | `4` | Global concurrency bound. `/ready` is false at capacity |
| `EIGER_LAUNCH_QUEUE_TIMEOUT_SECS` | `15` | Max wait for browser launch capacity |
| `EIGER_PER_SESSION_RSS_LIMIT_MB` | `1536` | Soft RSS ceiling for the browser process tree |
| `EIGER_MAX_SESSION_LIFETIME_SECS` | `1800` | Max lifetime before recycle |
| `EIGER_MAX_IDLE_TIME_SECS` | `300` | Max idle time before recycle |
| `EIGER_STEALTH_ENABLED` | `true` | Baseline stealth flags and script injection |

## API

`GET /health` returns process liveness only.

`GET /ready` returns readiness. It is ready only after Chrome has launched successfully at least once and the pool has capacity for a new session.

`GET /session` or `GET /` upgrades to a WebSocket, allocates a fresh Chromium session, proxies CDP to it, and recycles it when the WebSocket disconnects.

Pass `proxy=http://host:port` on `/session` or in session JSON bodies to launch Chrome with a single proxy server. This is a direct passthrough to Chrome. It does not rotate proxies or add credentials.

Pass `extensionPaths` in JSON bodies or as a comma-separated query value to load unpacked Chrome extensions. Paths must already exist on the host or container filesystem where Eiger runs. Eiger does not upload extension files. Modern Chrome-branded desktop builds block command-line extension loading. Use Chromium or Chrome for Testing for this path.

Set `EIGER_PROFILE_STORAGE_DIR` and pass `persistentProfileId` to reuse a Chrome profile between sessions. Sessions without `persistentProfileId` still use a fresh temporary profile.

`POST /sessions` pre-warms a browser session and returns an Eiger CDP WebSocket URL:

```bash
curl -X POST 'http://127.0.0.1:3000/sessions'
```

`POST /scrape` navigates a transient Chrome session and returns `{ html, title, url }`:

```bash
curl -X POST 'http://127.0.0.1:3000/scrape' \
  -H 'content-type: application/json' \
  -d '{"url":"https://example.com","waitUntil":"domcontentloaded","timeoutMs":30000}'
```

`POST /screenshot` returns raw PNG or JPEG bytes. It accepts `url`, `waitUntil`, `timeoutMs`, `fullPage`, and `format`.

`POST /pdf` returns raw PDF bytes. It accepts `url`, `waitUntil`, `timeoutMs`, `format`, `landscape`, and `printBackground`.

`GET /sessions` lists active sessions with state, age, RSS, and CPU.

`GET /sessions/view` renders the same session list as a plain HTML table with DevTools links.

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
