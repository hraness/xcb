import type { Metadata } from "next";
import { notFound } from "next/navigation";
import { AskAiAboutThis } from "@hraness/ui";
import { DocsShell } from "../docs-shell";
import { TopicContent } from "../topic-content";
import { docsTopics, findDocsTopic, topicMetadata } from "../topics";

export const dynamicParams = false;

export function generateStaticParams() {
  return docsTopics.map(({ slug }) => ({ slug }));
}

export async function generateMetadata({ params }: { params: Promise<{ slug: string }> }): Promise<Metadata> {
  const topic = findDocsTopic((await params).slug);
  if (!topic) notFound();
  return topicMetadata(topic);
}

export default async function DocsTopicPage({ params }: { params: Promise<{ slug: string }> }) {
  const topic = findDocsTopic((await params).slug);
  if (!topic) notFound();
  const index = docsTopics.findIndex((entry) => entry.slug === topic.slug);
  const previous = docsTopics[index - 1];
  const next = docsTopics[index + 1];
  return (
    <>
      <DocsShell active={topic.slug}>
        {topic.slug !== "reference" && (
          <header className="xcb-docs-heading">
            <p className="xcb-docs-eyebrow">{topic.group}</p>
            <h1>{topic.title}</h1>
            <p className="xcb-docs-lead">{topic.description}</p>
          </header>
        )}
        <article className="xcb-docs-body">
          <TopicContent slug={topic.slug} />
        </article>
        <nav className="xcb-docs-pagination" aria-label="More documentation">
          {previous ? <a href={`/docs/${previous.slug}`}><span>Previous</span>← {previous.title}</a> : <a href="/docs"><span>Previous</span>← Documentation</a>}
          {next && <a href={`/docs/${next.slug}`}><span>Next</span>{next.title} →</a>}
        </nav>
      </DocsShell>
      <AskAiAboutThis className="ask-ai" url={`https://xcb.dev/docs/${topic.slug}`} />
    </>
  );
}
