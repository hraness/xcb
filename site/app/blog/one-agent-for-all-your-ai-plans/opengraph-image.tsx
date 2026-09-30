import { socialCardAlt } from "../../social-cards";
import { socialImageFor, size } from "../../social-image";

export { contentType, size } from "../../social-image";

const path = "/blog/one-agent-for-all-your-ai-plans";

export function generateImageMetadata() {
  return [{ id: "card", size, alt: socialCardAlt(path) }];
}

export default async function OpengraphImage() {
  return socialImageFor(path);
}
