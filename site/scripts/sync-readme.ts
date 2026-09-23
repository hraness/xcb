import { resolve } from "node:path";

import { publicationMarkdown, publishedRelease, type PublishedRelease } from "../app/publication.ts";
import { readmeLanding, renderReadmeHtml, renderReadmeMarkdown } from "./readme-html.ts";

const siteRoot = resolve(import.meta.dir, "..");
const repositoryRoot = resolve(siteRoot, "..");

/** Keep repository installation guidance generic while publishing verified site links. */
export function readmeWithPublication(source: string, release: PublishedRelease | null): string {
  if (release === null) return source;
  const heading = /^### Install a verified release\r?$/gm;
  if ([...source.matchAll(heading)].length !== 1) {
    throw new Error("README must contain exactly one verified-release installation heading.");
  }
  return source.replace(heading, (match) => `${match}\n\n${publicationMarkdown(release)}`);
}

if (import.meta.main) {
  const source = readmeWithPublication(
    await Bun.file(resolve(repositoryRoot, "README.md")).text(),
    publishedRelease,
  );
  const landing = readmeLanding(source);
  const html = renderReadmeHtml(source);
  const markdown = renderReadmeMarkdown(source);
  await Bun.write(
    resolve(siteRoot, "app/readme.generated.ts"),
    "// Generated from ../README.md by scripts/sync-readme.ts. Do not edit.\n"
      + `export const readmeTitle = ${JSON.stringify(landing.title)};\n`
      + `export const readmeLead = ${JSON.stringify(landing.lead)};\n`
      + `export const readmeHtml = ${JSON.stringify(html)};\n`
      + `export const readmeMarkdown = ${JSON.stringify(markdown)};\n`,
  );
}
