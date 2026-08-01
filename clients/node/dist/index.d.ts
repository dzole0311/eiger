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
export interface CreateSessionOptions extends BrowserOverrides {
}
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
export declare class EigerError extends Error {
    readonly status: number;
    readonly body: string;
    constructor(message: string, status: number, body: string);
}
export declare class EigerClient {
    private readonly baseUrl;
    private readonly token?;
    private readonly fetchImpl;
    constructor(options?: EigerClientOptions);
    createSession(options?: CreateSessionOptions): Promise<CreatedSession>;
    getSession(id: string): Promise<SessionInfo>;
    listSessions(): Promise<SessionInfo[]>;
    deleteSession(id: string): Promise<void>;
    connect(options?: CreateSessionOptions): Promise<string>;
    sessionWebSocketUrl(id: string): string;
    scrape(request: PageRequest): Promise<ScrapeResult>;
    screenshot(request: ScreenshotRequest): Promise<BinaryResult>;
    pdf(request: PdfRequest): Promise<BinaryResult>;
    private requestJson;
    private requestBinary;
    private request;
    private url;
}
export default EigerClient;
