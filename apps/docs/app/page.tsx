"use client";

import { useEffect } from "react";

/**
 * The docs site is all docs — the root sends you to Getting Started.
 * Client-side redirect: with `output: "export"` there is no server to emit a
 * 3xx. On Cloudflare Pages the real 302 happens at the edge via
 * public/_redirects; this effect is the fallback (and what `next dev` uses).
 */
export default function Home() {
	useEffect(() => {
		window.location.replace("/docs");
	}, []);

	return (
		<p className="p-8 font-mono text-sm text-text-muted">
			Redirecting to{" "}
			<a href="/docs" className="text-brand">
				the docs
			</a>
			…
		</p>
	);
}
