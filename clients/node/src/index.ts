export type SessionState = 'launching' | 'ready' | 'in_use' | 'draining' | 'killing' | 'dead';

export interface EigerClientOptions {
  baseUrl?: string;
  token?: string;
  fetch?: typeof fetch;
}

export interface BrowserOverrides {
  stealthEnabled?: boolean;
  extraChromeArgs?: string[];
  proxy?: string;
  extensionPaths?: string[];
  persistentProfileId?: string;
}

export interface CreateSessionOptions extends BrowserOverrides {}

export interface CreatedSession {
  id: string;
  pid: number;
  cdpWsUrl: string;
  createdAt: string;
}

export interface SessionInfo {
  id: string;
  state: SessionState;
  pid?: number;
  createdAt: string;
  lastUsedAt: string;
  ageSeconds: number;
  idleSeconds: number;
  rssBytes?: number;
  cpuPercent?: number;
  processCount?: number;
  killReason?: string;
}

export interface PageRequest extends BrowserOverrides {
  url: string;
  waitUntil?: 'load' | 'domcontentloaded' | 'networkidle' | 'networkidle0' | 'networkalmostidle' | 'networkidle2';
  timeoutMs?: number;
}

export interface ScrapeResult {
  html: string;
  title: string;
  url: string;
}

export interface ScreenshotRequest extends PageRequest {
  fullPage?: boolean;
  format?: 'png' | 'jpeg';
}

export interface PdfRequest extends PageRequest {
  format?: 'Letter' | 'Legal' | 'Tabloid' | 'Ledger' | 'A0' | 'A1' | 'A2' | 'A3' | 'A4' | 'A5' | 'A6' | string;
  landscape?: boolean;
  printBackground?: boolean;
}

export interface BinaryResult {
  data: ArrayBuffer;
  contentType: string;
}

export class EigerError extends Error {
  readonly status: number;
  readonly body: string;

  constructor(message: string, status: number, body: string) {
    super(message);
    this.name = 'EigerError';
    this.status = status;
    this.body = body;
  }
}

export class EigerClient {
  private readonly baseUrl: URL;
  private readonly token?: string;
  private readonly fetchImpl: typeof fetch;

  constructor(options: EigerClientOptions = {}) {
    this.baseUrl = new URL(options.baseUrl ?? 'http://127.0.0.1:3000');
    this.token = options.token;
    this.fetchImpl = options.fetch ?? globalThis.fetch;

    if (!this.fetchImpl) {
      throw new Error('fetch is not available; use Node 18 or pass a fetch implementation');
    }
  }

  async createSession(options: CreateSessionOptions = {}): Promise<CreatedSession> {
    return this.requestJson<CreatedSession>('/sessions', {
      method: 'POST',
      body: JSON.stringify(options),
    });
  }

  async getSession(id: string): Promise<SessionInfo> {
    return this.requestJson<SessionInfo>(`/sessions/${encodeURIComponent(id)}`);
  }

  async listSessions(): Promise<SessionInfo[]> {
    return this.requestJson<SessionInfo[]>('/sessions');
  }

  async deleteSession(id: string): Promise<void> {
    await this.request(`/sessions/${encodeURIComponent(id)}`, { method: 'DELETE' });
  }

  async connect(options: CreateSessionOptions = {}): Promise<string> {
    const session = await this.createSession(options);
    return session.cdpWsUrl;
  }

  sessionWebSocketUrl(id: string): string {
    const url = this.url(`/sessions/${encodeURIComponent(id)}/cdp`);
    url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
    if (this.token) {
      url.searchParams.set('token', this.token);
    }
    return url.toString();
  }

  async scrape(request: PageRequest): Promise<ScrapeResult> {
    return this.requestJson<ScrapeResult>('/scrape', {
      method: 'POST',
      body: JSON.stringify(request),
    });
  }

  async screenshot(request: ScreenshotRequest): Promise<BinaryResult> {
    return this.requestBinary('/screenshot', request);
  }

  async pdf(request: PdfRequest): Promise<BinaryResult> {
    return this.requestBinary('/pdf', request);
  }

  private async requestJson<T>(path: string, init: RequestInit = {}): Promise<T> {
    const response = await this.request(path, init);
    return response.json() as Promise<T>;
  }

  private async requestBinary(path: string, body: unknown): Promise<BinaryResult> {
    const response = await this.request(path, {
      method: 'POST',
      body: JSON.stringify(body),
    });
    return {
      data: await response.arrayBuffer(),
      contentType: response.headers.get('content-type') ?? 'application/octet-stream',
    };
  }

  private async request(path: string, init: RequestInit = {}): Promise<Response> {
    const headers = new Headers(init.headers);
    if (init.body && !headers.has('content-type')) {
      headers.set('content-type', 'application/json');
    }
    if (this.token && !headers.has('authorization')) {
      headers.set('authorization', `Bearer ${this.token}`);
    }

    const response = await this.fetchImpl(this.url(path), {
      ...init,
      headers,
    });

    if (!response.ok) {
      const body = await response.text();
      throw new EigerError(`Eiger request failed: ${response.status}`, response.status, body);
    }

    return response;
  }

  private url(path: string): URL {
    return new URL(path, this.baseUrl);
  }
}

export default EigerClient;
