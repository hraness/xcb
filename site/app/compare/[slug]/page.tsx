import type { Metadata } from "next";
import { notFound } from "next/navigation";
import { ComparisonPage } from "../comparison-page";
import { comparisons, findComparison } from "../comparisons";

export const dynamicParams = false;

export function generateStaticParams() {
  return comparisons.map(({ slug }) => ({ slug }));
}

export async function generateMetadata({ params }: Readonly<{ params: Promise<{ slug: string }> }>): Promise<Metadata> {
  const entry = findComparison((await params).slug);
  if (entry === undefined) return {};
  const url = `/compare/${entry.slug}`;
  return {
    title: entry.title,
    description: entry.description,
    alternates: { canonical: url },
    openGraph: { title: entry.title, description: entry.description, siteName: "Excalibur (xcb)", type: "article", url },
    twitter: { card: "summary_large_image", title: entry.title, description: entry.description },
  };
}

export default async function Comparison({ params }: Readonly<{ params: Promise<{ slug: string }> }>) {
  const entry = findComparison((await params).slug);
  if (entry === undefined) notFound();
  return <ComparisonPage entry={entry} />;
}
