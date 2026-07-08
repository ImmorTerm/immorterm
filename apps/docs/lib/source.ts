import { loader } from "fumadocs-core/source";
import { createMDXSource } from "fumadocs-mdx";
import { docs, meta } from "@/.source";

// The content source the layout + pages read. Docs are served under /docs.
export const source = loader({
	baseUrl: "/docs",
	source: createMDXSource(docs, meta),
});
