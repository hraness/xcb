import { socialCardAlt } from "../../social-cards";
import { socialImageFor, size } from "../../social-image";

type Params = { slug: string };

export { contentType, size } from "../../social-image";

/**
 * One card per page, with that page's alt text. The page's metadata passes the
 * slug; the image route's own build step lists ids before any slug is known,
 * so the card itself renders on first request.
 */
export function generateImageMetadata({ params }: { params: Partial<Params> }) {
  return [{ id: "card", size, ...(params.slug === undefined ? {} : { alt: socialCardAlt(`/docs/${params.slug}`) }) }];
}

export default async function OpengraphImage({ params }: { params: Promise<Params> }) {
  return socialImageFor(`/docs/${(await params).slug}`);
}
