import { renderInstallScript } from "./render";

/** Serves https://xcb.sh/install.sh, rendered once at build time. */
export const dynamic = "force-static";

const body = renderInstallScript();

export function GET(): Response {
  return new Response(body, {
    headers: {
      // Plain text, so a browser shows the script for reading before you pipe it.
      "cache-control": "public, max-age=300",
      "content-type": "text/plain; charset=utf-8",
      "x-content-type-options": "nosniff",
    },
  });
}
