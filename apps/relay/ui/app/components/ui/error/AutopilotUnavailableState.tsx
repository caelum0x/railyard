import { AlertCircle } from "lucide-react";
import { Link } from "react-router";
import { Card, CardContent, CardHeader, CardTitle } from "~/components/ui/card";

/**
 * Autopilot is an optional upstream integration backed by TensorZero's hosted
 * service. Relay does not depend on it: the sidebar hides Autopilot unless the
 * gateway reports it configured, and direct visits land here.
 */
export function AutopilotUnavailableState() {
  return (
    <div className="grid min-h-full place-items-center p-4">
      <Card className="max-w-md">
        <CardHeader>
          <div className="flex items-center gap-2">
            <AlertCircle className="h-5 w-5 text-amber-500" aria-hidden />
            <CardTitle>Autopilot is not enabled</CardTitle>
          </div>
        </CardHeader>
        <CardContent>
          <p className="text-muted-foreground text-sm">
            Autopilot is an optional integration with an externally hosted
            service and is disabled on this Relay deployment. Every Relay
            gateway, billing, and metering feature works without it.
          </p>
          <p className="text-muted-foreground mt-4 text-sm text-balance">
            An operator can enable it by setting{" "}
            <code className="bg-muted rounded px-1 py-0.5 font-mono">
              TENSORZERO_AUTOPILOT_API_KEY
            </code>{" "}
            on the gateway.
          </p>
          <p className="text-muted-foreground mt-4 text-sm">
            <Link to="/" className="hover:text-primary underline">
              Back to the overview
            </Link>
          </p>
        </CardContent>
      </Card>
    </div>
  );
}
