import type { ArticleVideoRecord } from "@hraness/design-kit";

/**
 * The launch film embedded at the top of the launch post, or null until its
 * files are rendered into public/launch/. The source is in video/ at the repo
 * root; video/README.md has the render and deliver commands. While this is
 * null the post opens with the interactive showcase instead, so the page never
 * points at a file that does not exist.
 */
export const launchFilm: Readonly<{ caption: string; video: ArticleVideoRecord }> | null = null;
