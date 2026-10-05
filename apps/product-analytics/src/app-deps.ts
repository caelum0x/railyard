// What every router receives. Built once by app.ts.

import type { Meter } from "./capture/meter.ts";
import type { CapturePipeline } from "./capture/pipeline.ts";
import type { UsageCounter } from "./capture/usage.ts";
import type { EventWriter } from "./capture/writer.ts";
import type { Db } from "./db.ts";
import type { FlagsReader } from "./flags/client.ts";
import type { GdprHooks } from "./gdpr/hooks.ts";
import type { Logger } from "./log.ts";
import type { SiteDirectory } from "./tenancy/site-directory.ts";
import type { Mailer } from "./alerts/mail.ts";

export interface ServiceDeps {
  db: Db;
  sites: SiteDirectory;
  writer: EventWriter;
  usage: UsageCounter;
  meter: Meter;
  pipeline: CapturePipeline;
  flags: FlagsReader;
  hooks: GdprHooks;
  log: Logger;
  now: () => Date;
  consoleSecret: string | null;
  bridgeSecret: string | null;
  /** Alert and report mail (QY1.4): Rally Send, or none (logged as not configured). */
  mailer: Mailer;
}
