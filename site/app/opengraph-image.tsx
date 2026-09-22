import {
  createSocialImageResponse,
  socialImageContentType as contentType,
  socialImageSize as size,
} from "@hraness/web-discovery/social-image";

export const alt = "xcb — One router for your AI coding subscriptions.";
export { contentType, size };

function XcbMark() {
  return (
    <svg aria-label="xcb mark" fill="none" height="42" role="img" viewBox="0 0 42 42" width="42">
      <path d="M21 4v33M9 15h24" stroke="currentColor" strokeLinecap="round" strokeWidth="4" />
    </svg>
  );
}

export default function OpengraphImage() {
  return createSocialImageResponse({
    description: "One bounded turn across your own coding-agent accounts.",
    domain: "xcb.sh",
    eyebrow: "xcb / Excalibur",
    mark: <XcbMark />,
    theme: {
      accent: "#8A5A28",
      background: "#F8F7F4",
      foreground: "#1C1A18",
      muted: "#6A655E",
    },
    title: "All your AI subscriptions. One router.",
  });
}
