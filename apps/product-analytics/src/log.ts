// One-line JSON logs. Never pass request headers or client addresses here:
// the service stores and logs no IP address (tested).

export interface Logger {
  info(msg: string, fields?: Record<string, unknown>): void;
  error(msg: string, fields?: Record<string, unknown>): void;
}

export function jsonLogger(write: (line: string) => void = (l) => process.stdout.write(`${l}\n`)): Logger {
  const out = (level: string, msg: string, fields: Record<string, unknown> = {}) =>
    write(JSON.stringify({ level, msg, ...fields, at: new Date().toISOString() }));
  return { info: (m, f) => out("info", m, f), error: (m, f) => out("error", m, f) };
}

export const silentLogger: Logger = { info: () => undefined, error: () => undefined };
