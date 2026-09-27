import { highlightCode } from "@hraness/design-kit/syntax-highlighting";
import { CopyButton } from "@hraness/ui";

type Language = "shell" | "json" | "text";

/** Shell prompts are shown for reading and dropped when copying. */
function copyText(code: string, language: Language): string {
  if (language !== "shell") return code;
  return code
    .split("\n")
    .map((line) => line.replace(/^\$ /u, ""))
    .join("\n");
}

/**
 * One highlighted, copyable code block. The design kit highlights with CSS
 * classes (no inline styles), and the copy button copies exactly what the
 * block shows, minus shell prompts.
 */
export function CodeBlock({
  code,
  language = "shell",
  copyLabel = "Copy",
}: Readonly<{ code: string; language?: Language; copyLabel?: string }>) {
  const highlighted = highlightCode(code, language, { styles: "classes" });
  return (
    <div className="xcb-code">
      <pre className={`xcb-code-block ${highlighted.className}`} tabIndex={0}>
        <code dangerouslySetInnerHTML={{ __html: highlighted.html }} />
      </pre>
      <CopyButton className="xcb-code-copy" copyLabel={copyLabel} value={copyText(code, language)} />
    </div>
  );
}
