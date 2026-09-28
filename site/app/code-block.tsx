import { MarketingProofFrame, SyntaxCode } from "@hraness/design-kit/react/server";
import type { SyntaxLanguage } from "@hraness/design-kit/syntax-highlighting";
import { CopyButton } from "@hraness/ui";

/** Shell prompts are shown for reading and dropped when copying. */
function copyText(code: string, language: SyntaxLanguage): string {
  if (language !== "shell") return code;
  return code
    .split("\n")
    .map((line) => line.replace(/^\$ /u, ""))
    .join("\n");
}

/**
 * One highlighted, copyable code block. The design kit highlights with CSS
 * classes (no inline styles), and the copy button copies exactly what the
 * block shows, minus shell prompts. Transcripts can supply the command to
 * copy separately so their output is never pasted into a shell.
 */
export function CodeBlock({
  code,
  language = "shell",
  copyLabel = "Copy",
  copyValue,
  terminal = language === "shell",
}: Readonly<{ code: string; language?: SyntaxLanguage; copyLabel?: string; copyValue?: string; terminal?: boolean }>) {
  const content = (
    <>
      <pre className={`xcb-code-block language-${language}`} tabIndex={0}>
        <SyntaxCode code={code} language={language} styles="classes" />
      </pre>
      <CopyButton className="xcb-code-copy" copyLabel={copyLabel} value={copyValue ?? copyText(code, language)} />
    </>
  );
  return terminal
    ? <MarketingProofFrame className="xcb-code xcb-code--terminal" title="Terminal">{content}</MarketingProofFrame>
    : <div className="xcb-code">{content}</div>;
}
