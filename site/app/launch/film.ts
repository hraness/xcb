import type { ArticleVideoRecord } from "@hraness/design-kit";

/**
 * The launch film embedded at the top of the launch post. The source is in
 * video/ at the repo root; video/README.md has the render and deliver
 * commands, which write the files below into public/media/. The 1:1 cut
 * (`/media/xcb-launch-1x1.mp4`) is for social posts and is not embedded.
 * tests/launch.test.ts fails when any file named here is missing.
 */
export const launchFilm: Readonly<{ video: ArticleVideoRecord }> | null = null;
