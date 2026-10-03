import { readmeMarkdown } from "../readme.generated";

// The README is machine-readable at https://xcb.sh/README.md, as llms.txt
// advertises. It is served from the synced source so it cannot drift from the
// rendered reference page.
export const dynamic = "force-static";

export function GET(): Response {
  return new Response(readmeMarkdown, {
    headers: {
      "cache-control": "public, max-age=3600",
      "content-type": "text/markdown; charset=utf-8",
    },
  });
}
