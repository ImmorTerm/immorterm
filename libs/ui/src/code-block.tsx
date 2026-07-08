"use client";

import { Check, Copy, Terminal } from "lucide-react";
import { useState } from "react";

export interface CodeBlockProps {
	/** Shell lines, rendered one per row with a `$` prompt. */
	lines: string[];
	/** Optional label above the block. */
	label?: string;
	className?: string;
}

/** Copy-to-clipboard shell snippet, styled as a terminal command. */
export function CodeBlock({ lines, label, className }: CodeBlockProps) {
	const [copied, setCopied] = useState(false);

	const copy = () => {
		navigator.clipboard.writeText(lines.join("\n"));
		setCopied(true);
		setTimeout(() => setCopied(false), 2000);
	};

	return (
		<div className={className}>
			{label && (
				<div className="mb-2 text-xs font-mono uppercase tracking-wider text-text-muted">
					{label}
				</div>
			)}
			<button
				type="button"
				onClick={copy}
				className="group flex w-full items-center justify-between gap-4 rounded-2xl border border-white/10 bg-bg-card px-5 py-4 text-left transition-all hover:border-brand/40"
			>
				<div className="min-w-0 font-mono text-sm">
					{lines.map((line) => (
						<div key={line} className="flex items-center gap-3 py-0.5">
							<span className="text-text-muted select-none">$</span>
							<span className="truncate text-accent">{line}</span>
						</div>
					))}
				</div>
				<span className="flex shrink-0 items-center gap-2 font-mono text-xs text-text-muted">
					{copied ? (
						<>
							<Check className="h-4 w-4 text-success" />
							Copied
						</>
					) : (
						<>
							<Copy className="h-4 w-4 transition-colors group-hover:text-text-primary" />
							Copy
						</>
					)}
				</span>
			</button>
		</div>
	);
}

/** Re-export of the terminal icon used alongside install commands. */
export const CodeBlockIcon = Terminal;
