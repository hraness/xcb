import type { ReactNode } from "react";

/**
 * The one heading composition for every interior page (compare, download,
 * reflexes, blog index, docs). The decorative hero field is a marketing
 * surface; interior pages use this ordinary heading instead.
 */
export function PageHeader({
  id,
  title,
  lead,
  eyebrow,
  meta,
  children,
}: Readonly<{
  id: string;
  title: string;
  lead?: ReactNode;
  eyebrow?: ReactNode;
  meta?: ReactNode;
  children?: ReactNode;
}>) {
  return (
    <header className="xcb-page-header" aria-labelledby={id}>
      {eyebrow === undefined ? null : <p className="xcb-page-eyebrow">{eyebrow}</p>}
      <h1 id={id}>{title}</h1>
      {lead === undefined ? null : <p className="xcb-page-lead">{lead}</p>}
      {meta === undefined ? null : <p className="xcb-page-meta">{meta}</p>}
      {children}
    </header>
  );
}

/** A plain page section: one h2 at the shared level token, no rule, no card. */
export function PageSection({
  id,
  title,
  children,
  wide = false,
}: Readonly<{ id: string; title: string; children: ReactNode; wide?: boolean }>) {
  return (
    <section className={wide ? "xcb-page-section xcb-page-section--wide" : "xcb-page-section"} id={id} aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`}>{title}</h2>
      {children}
    </section>
  );
}
