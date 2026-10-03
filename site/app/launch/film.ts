import type { ArticleVideoRecord } from "@hraness/design-kit";

/**
 * The launch film embedded at the top of the launch post. The source is in
 * video/ at the repo root; video/README.md has the render and deliver
 * commands, which write the files below into public/media/. The 1:1 cut
 * (`/media/xcb-launch-1x1.mp4`) is for social posts and is not embedded.
 * tests/launch.test.ts fails when any file named here is missing.
 */
export const launchFilm: Readonly<{ video: ArticleVideoRecord }> | null = {
  video: {
    name: "Introducing Excalibur",
    description:
      "A captioned introduction to xcb: choose an account for a task, favor quota near its reset, continue on an available account after a reported usage limit, and follow work across machines.",
    sources: [
      { src: "/media/xcb-launch.webm", type: "video/webm" },
      { src: "/media/xcb-launch.mp4", type: "video/mp4" },
    ],
    poster: "/media/xcb-launch-poster.jpg",
    captions: "/media/xcb-launch.vtt",
    width: 1920,
    height: 1080,
    duration: "PT42.1S",
    uploadDate: "2026-10-01",
  },
};
