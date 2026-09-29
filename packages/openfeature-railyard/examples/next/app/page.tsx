// Example Next.js page (Wave P PY2.1 acceptance 1): rendered on the server with
// the flag's current state. A flip shows on the next request within 60 s (the
// provider keeps an answer 15 s). NEXT_PUBLIC_FLAGS_KEY is written to .env
// when flags are turned on for the project.
import { RailyardServerFlagsProvider } from "@railyard/openfeature";

const provider = new RailyardServerFlagsProvider({ key: process.env.NEXT_PUBLIC_FLAGS_KEY ?? "" });

export const dynamic = "force-dynamic";

export default async function Page() {
  const { value: on } = await provider.resolveBooleanEvaluation("new_checkout", false);
  return <main>{on ? "New checkout is on" : "Classic checkout"}</main>;
}
