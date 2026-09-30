"use client";
import { CopyButton } from "@hraness/ui";
import type { ComponentProps } from "react";
import { captureInstallCopied } from "./site-analytics";

export function AnalyticsCopyButton(props: ComponentProps<typeof CopyButton>) {
  return <CopyButton {...props} onCopySuccess={() => {
    props.onCopySuccess?.();
    const command = typeof props.value === "string" ? props.value.trim() : "";
    if (/^(?:curl\s|brew install\s|git clone https:\/\/github\.com\/hraness\/xcb)/.test(command)) {
      captureInstallCopied(command.startsWith("curl") ? "curl" : command.startsWith("brew") ? "brew" : "other");
    }
  }} />;
}
