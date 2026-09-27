import type { ReactNode } from "react";

/** `code` in backticks, or [a label](href). The compare data allows nothing else. */
const inlineMark = /`([^`]+)`|\[([^\]]+)\]\(([^)\s]+)\)/gu;

/** Links that leave xcb.sh; only these carry ↗ (AGENTS.md "Public copy"). */
export function isExternal(href: string): boolean {
  return /^https?:\/\//u.test(href);
}

/** A link with ↗ when it leaves xcb.sh and no glyph when it stays. */
export function TextLink({ href, children }: Readonly<{ href: string; children: ReactNode }>) {
  return <a href={href}>{children}{isExternal(href) ? " ↗" : null}</a>;
}

function marks(text: string, links: boolean): ReactNode[] {
  const parts: ReactNode[] = [];
  let last = 0;
  for (const match of text.matchAll(inlineMark)) {
    const index = match.index;
    const [whole, code, label, href] = match;
    if (index > last) parts.push(text.slice(last, index));
    if (code !== undefined) parts.push(<code key={index}>{code}</code>);
    else if (links && label !== undefined && href !== undefined) parts.push(<TextLink key={index} href={href}>{marks(label, false)}</TextLink>);
    else parts.push(whole);
    last = index + whole.length;
  }
  if (last < text.length) parts.push(text.slice(last));
  return parts;
}

/** Renders one compare text field with its inline code and links. */
export function RichText({ text }: Readonly<{ text: string }>) {
  return <>{marks(text, true)}</>;
}
