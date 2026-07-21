import { docs, meta } from "@/.source/server";
import { loader, type Source } from "fumadocs-core/source";
import { toFumadocsSource } from "fumadocs-mdx/runtime/server";

// The content source the layout + pages read. Docs are served under /docs.
// fumadocs-mdx 15 emits server-runtime collections (.source/server.ts); wrap
// them with toFumadocsSource instead of the old createMDXSource.
//
// The `as Source<...>` is load-bearing: this workspace resolves multiple
// physical copies of fumadocs-core@16 (fumadocs-mdx and this app land on
// different peer-hash instantiations), so `toFumadocsSource`'s Source is a
// nominally different type than loader's. Without the assertion, loader can't
// infer pageData across copies and `page.data` collapses to bare PageData
// (no `.body`/`.toc`). Runtime is unaffected — the JS is structural.
export const source = loader({
	baseUrl: "/docs",
	source: toFumadocsSource(docs, meta) as Source<{
		pageData: (typeof docs)[number];
		metaData: (typeof meta)[number];
	}>,
});
