import { fileURLToPath } from "node:url";
import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  outputFileTracingRoot: fileURLToPath(new URL(".", import.meta.url)),
  async redirects() {
    return [
      {
        source: "/alternatives/:path*",
        has: [{ type: "host", value: "hra.sh" }],
        destination: "https://xcb.sh/compare",
        permanent: true,
      },
      {
        source: "/download",
        has: [{ type: "host", value: "hra.sh" }],
        destination: "https://xcb.sh/download",
        permanent: true,
      },
      {
        source: "/:path*",
        has: [{ type: "host", value: "hra.sh" }],
        destination: "https://xcb.sh/:path*",
        permanent: true,
      },
    ];
  },
};

export default nextConfig;
