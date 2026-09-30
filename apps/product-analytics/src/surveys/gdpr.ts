// Privacy hook for survey answers kept outside pa.events. Owned by A3
// (handed over by A1: "then"). Survey answers sent as events are already
// removed with the person's events.

import type { GdprHook } from "../gdpr/hooks.ts";

export const surveysGdprHook: GdprHook = {
  name: "survey_answers",
  deletePerson: async () => 0,
  exportPerson: async () => [],
};
