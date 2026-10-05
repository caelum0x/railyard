// What the alert and subscription passes need to mail (QY1.4).

import type { ServiceDeps } from "../app-deps.ts";
import type { SendDeps } from "./send.ts";

export { runAlerts } from "./alerts.ts";

export const DEFAULT_CONSOLE_URL = "https://railyard.ereboslab.com";

/** PA_PUBLIC_URL (for unsubscribe links) and PA_CONSOLE_URL (for "open" links). */
export function mailDeps(deps: Pick<ServiceDeps, "db" | "mailer" | "consoleSecret">, env: Record<string, string | undefined> = process.env): SendDeps & { consoleUrl: string } {
  return {
    db: deps.db,
    mailer: deps.mailer,
    secret: deps.consoleSecret,
    publicUrl: env.PA_PUBLIC_URL?.trim() || "https://pa.railyard.ereboslab.com",
    consoleUrl: env.PA_CONSOLE_URL?.trim() || DEFAULT_CONSOLE_URL,
  };
}
