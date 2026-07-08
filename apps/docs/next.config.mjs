import { createMDX } from "fumadocs-mdx/next";

const withMDX = createMDX();

/** @type {import('next').NextConfig} */
const config = {
	reactStrictMode: true,
	// Fully static site → Cloudflare Pages serves the exported `out/` dir with no
	// runtime. Search is prebuilt too (staticGET in app/api/search/route.ts).
	// `next dev` ignores `output: export`, so the Tilt/portless preview is unaffected.
	output: "export",
	images: { unoptimized: true },
	// @immorterm/ui is source-shipped (main: ./src/index.ts) — Next must transpile
	// it and its workspace deps, same as apps/web/next.config.js.
	transpilePackages: ["@immorterm/ui", "@immorterm/menu-data", "@immorterm/tier-config"],
};

export default withMDX(config);
