import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  // A self-contained server for the container image (Dockerfile).
  output: "standalone",
  poweredByHeader: false,
};

export default nextConfig;
