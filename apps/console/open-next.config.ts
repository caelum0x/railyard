import { defineCloudflareConfig } from "@opennextjs/cloudflare";

// Every console page is personalised; there is nothing to cache incrementally.
export default defineCloudflareConfig({});
