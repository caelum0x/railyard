"use client";

/**
 * Autopilot Available Context Provider
 *
 * This module provides a React Context for checking if autopilot is available.
 * Relay does not offer Autopilot (it needs TensorZero's hosted service), so the
 * root loader always provides `false` and Autopilot entry points stay hidden.
 *
 * @module autopilot-available
 */

import { createContext, use } from "react";

const AutopilotAvailableContext = createContext(false);
AutopilotAvailableContext.displayName = "AutopilotAvailableContext";

/**
 * Hook to check if autopilot is available
 */
export function useAutopilotAvailable() {
  return use(AutopilotAvailableContext);
}

export function AutopilotAvailableProvider({
  children,
  value,
}: {
  children: React.ReactNode;
  value: boolean;
}) {
  return (
    <AutopilotAvailableContext value={value}>
      {children}
    </AutopilotAvailableContext>
  );
}
