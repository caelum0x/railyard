import { redirect } from "next/navigation";

export const dynamic = "force-dynamic";

// The plan's path for this screen; the screen lives under Insights.
export default function SegmentsRedirect({ searchParams }: { searchParams: { app?: string } }) {
  redirect(`/product-analytics/insights/segments${searchParams.app ? `?app=${encodeURIComponent(searchParams.app)}` : ""}`);
}
