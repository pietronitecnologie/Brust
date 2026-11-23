// TypeScript declarations for the Brust web SDK (brust.js) and for the
// global `window.brust` object injected by the Brust client.

export interface EventMeta {
  /** Event name (useful with the "*" listener). */
  event: string;
  /** Who emitted it: an app id, "external:<name>" or "brust". */
  source?: string;
}

export interface HandlerContext {
  /** Caller: an app id or "external:<name>". */
  from: string;
}

export type EventListener<T = any> = (data: T, meta: EventMeta) => void;
export type MethodHandler<P = any, R = any> = (params: P, ctx: HandlerContext) => R | Promise<R>;
export type Unsubscribe = () => void;

export interface ExecOptions {
  cwd?: string;
  env?: Record<string, string>;
  stdin?: string;
  timeout_ms?: number;
}

export interface ExecResult {
  code: number | null;
  stdout: string;
  stderr: string;
  timed_out: boolean;
}

export interface BridgeClient {
  id: number;
  name: string;
}

export interface PrintPageOptions {
  /** Printer name (see `print.printers()`); default printer if omitted. */
  printer?: string;
  /** Print without a dialog (default true; silent printing is supported on Windows, elsewhere the dialog is shown). */
  silent?: boolean;
  [key: string]: unknown;
}

/** The global object injected by Brust into app pages. */
export interface BrustBridge {
  readonly version: string;
  readonly appId: string;
  readonly paneId: number;
  invoke<T = any>(cmd: string, args?: Record<string, unknown>): Promise<T>;
  on<T = any>(event: string, fn: EventListener<T>): Unsubscribe;
  off(event: string, fn: EventListener): void;
  emit(event: string, data?: unknown, target?: string | null): void;
  handle<P = any, R = any>(method: string, fn: MethodHandler<P, R>): Unsubscribe;
  call<T = any>(target: string, method: string, params?: unknown, opts?: { timeout_ms?: number }): Promise<T>;
  exec(program: string, args?: string[], opts?: ExecOptions): Promise<ExecResult>;
  open(target: string): Promise<null>;
  info<T = any>(): Promise<T>;
}

declare global {
  interface Window {
    /** Present only inside Brust, on the app's own origins. */
    brust?: BrustBridge;
  }
  interface WindowEventMap {
    "brust:ready": CustomEvent<BrustBridge>;
  }
}

/** True when `window.brust` is available right now. */
export function inBrust(): boolean;
/** Resolves true inside Brust, false after `timeoutMs` in a normal browser. */
export function ready(timeoutMs?: number): Promise<boolean>;
/** Version, app id and pane id, or null outside Brust. */
export function context(): { version: string; appId: string; paneId: number } | null;

export function invoke<T = any>(cmd: string, args?: Record<string, unknown>): Promise<T>;
export function call<T = any>(target: string, method: string, params?: unknown, options?: { timeoutMs?: number }): Promise<T>;
export function emit(event: string, data?: unknown, target?: string | null): void;
export function on<T = any>(event: string, fn: EventListener<T>): Unsubscribe;
export function once<T = any>(event: string, fn: EventListener<T>): Unsubscribe;
export function handle<P = any, R = any>(method: string, fn: MethodHandler<P, R>): Unsubscribe;
export function exec(program: string, args?: string[], options?: ExecOptions): Promise<ExecResult>;
export function info<T = any>(): Promise<T>;

/** Tracks connected local programs; `onChange` receives the connected names. Resolves to a stop function. */
export function watchClients(onChange?: (names: string[]) => void): Promise<Unsubscribe>;
export function isConnected(name: string): boolean;

export declare const notes: {
  append(text: string, notebook?: string): Promise<any>;
};

export declare const ui: {
  notify(message: string, title?: string): Promise<any>;
  badge(count: number): Promise<any>;
  setTitle(title: string): Promise<any>;
  open(appOrOptions: string | { app?: string; url?: string }): Promise<any>;
  focus(): Promise<any>;
};

export declare const print: {
  page(options?: PrintPageOptions): Promise<any>;
  printers<T = any>(): Promise<T>;
};

declare const brust: {
  ready: typeof ready;
  inBrust: typeof inBrust;
  context: typeof context;
  invoke: typeof invoke;
  call: typeof call;
  emit: typeof emit;
  on: typeof on;
  once: typeof once;
  handle: typeof handle;
  exec: typeof exec;
  info: typeof info;
  watchClients: typeof watchClients;
  isConnected: typeof isConnected;
  notes: typeof notes;
  ui: typeof ui;
  print: typeof print;
};
export default brust;
