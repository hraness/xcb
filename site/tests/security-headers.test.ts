import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { contentSecurityPolicy, securityHeaders } from "../security-headers";

describe("security headers", () => {
  test("forbids framing, plugins, and foreign origins", () => {
    const csp = contentSecurityPolicy();
    expect(csp).toContain("frame-ancestors 'none'");
    expect(csp).toContain("object-src 'none'");
    expect(csp).toContain("default-src 'self'");
    expect(csp).not.toContain("'unsafe-eval'");
    expect(contentSecurityPolicy(true)).toContain("'unsafe-eval'");
  });

  test("sets the baseline transport and sniffing headers", () => {
    const keys = securityHeaders().map((header) => header.key);
    for (const key of [
      "Content-Security-Policy",
      "X-Content-Type-Options",
      "Referrer-Policy",
      "Permissions-Policy",
      "Strict-Transport-Security",
    ]) {
      expect(keys).toContain(key);
    }
  });

  test("security.txt names a reporting route and an unexpired date", () => {
    const text = readFileSync(new URL("../public/.well-known/security.txt", import.meta.url), "utf8");
    expect(text).toMatch(/^Contact: https:\/\/github\.com\/hraness\/xcb\/security\/advisories\/new$/m);
    const expires = /^Expires: (.+)$/m.exec(text)?.[1] ?? "";
    expect(Date.parse(expires)).toBeGreaterThan(Date.now());
  });
});
