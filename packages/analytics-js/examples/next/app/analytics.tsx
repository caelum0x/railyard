"use client";
// Next.js: create once, grant from your consent banner.
import { createAnalytics } from "@railyard/analytics-js";

export const analytics = typeof window === "undefined" ? null : createAnalytics();

export function ConsentButtons() {
  return (
    <div>
      <button onClick={() => analytics?.setConsent("granted")}>Allow analytics</button>
      <button onClick={() => analytics?.setConsent("denied")}>No thanks</button>
    </div>
  );
}
