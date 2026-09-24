import {
  data,
  isRouteErrorResponse,
  Outlet,
  type RouteHandle,
} from "react-router";
import type { Route } from "./+types/layout";
import { AutopilotUnavailableState } from "~/components/ui/error/AutopilotUnavailableState";
import { LayoutErrorBoundary } from "~/components/ui/error";
import { isAutopilotUnavailableError } from "~/utils/tensorzero/errors";

export const handle: RouteHandle = {
  crumb: () => [{ label: "Autopilot", noLink: true }],
};

const AUTOPILOT_UNAVAILABLE_ERROR = "Autopilot Unavailable";

// Autopilot needs TensorZero's hosted Autopilot service, which Relay does not
// offer. Every Autopilot route answers with the "not available" state, even if
// a gateway still carries an upstream Autopilot key.
// eslint-disable-next-line @typescript-eslint/require-await
export async function loader() {
  throw data({ errorType: AUTOPILOT_UNAVAILABLE_ERROR }, { status: 501 });
}

export default function AutopilotLayout() {
  return <Outlet />;
}

export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  // Autopilot unavailable gets special treatment
  if (
    isRouteErrorResponse(error) &&
    error.data?.errorType === AUTOPILOT_UNAVAILABLE_ERROR
  ) {
    return <AutopilotUnavailableState />;
  }
  if (isAutopilotUnavailableError(error)) {
    return <AutopilotUnavailableState />;
  }

  // All other errors (including infra) handled by LayoutErrorBoundary
  return <LayoutErrorBoundary error={error} />;
}
