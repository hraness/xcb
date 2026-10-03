import { ATOM_FEED_CONTENT_TYPE } from "@hraness/web-discovery";
import { blogAtomFeed } from "../feed";

export const dynamic = "force-static";

export function GET(): Response {
  return new Response(blogAtomFeed(), {
    headers: {
      "cache-control": "public, max-age=3600",
      "content-type": ATOM_FEED_CONTENT_TYPE,
    },
  });
}
