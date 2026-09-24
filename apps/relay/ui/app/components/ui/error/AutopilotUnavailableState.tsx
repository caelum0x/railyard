import { AlertCircle } from "lucide-react";
import { Link } from "react-router";
import { Card, CardContent, CardHeader, CardTitle } from "~/components/ui/card";

/**
 * Autopilot was upstream's paid automated-AI-engineer product, run on
 * TensorZero's hosted service. Relay does not offer it: the sidebar and every
 * "Ask Autopilot" button are hidden, and direct visits land here.
 */
export function AutopilotUnavailableState() {
  return (
    <div className="grid min-h-full place-items-center p-4">
      <Card className="max-w-md">
        <CardHeader>
          <div className="flex items-center gap-2">
            <AlertCircle className="h-5 w-5 text-amber-500" aria-hidden />
            <CardTitle>Autopilot is not available in Relay</CardTitle>
          </div>
        </CardHeader>
        <CardContent>
          <p className="text-muted-foreground text-sm">
            Autopilot depended on a hosted service operated by the upstream
            TensorZero project. That service is not part of Relay, so this
            feature is turned off. Every Relay gateway, observability,
            evaluation, fine-tuning, billing, and metering feature works
            without it.
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
