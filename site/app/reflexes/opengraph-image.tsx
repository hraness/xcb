import { socialCardAlt } from "../social-cards";
import { socialImageFor } from "../social-image";

export { contentType, size } from "../social-image";
export const alt = socialCardAlt("/reflexes");

export default function OpengraphImage() {
  return socialImageFor("/reflexes");
}
