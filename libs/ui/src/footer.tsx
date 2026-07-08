import type { ReactNode } from "react";
import { cn } from "./cn";

export interface FooterColumn {
	title: string;
	links: { label: string; href: string }[];
}

export interface FooterProps {
	logo?: ReactNode;
	columns?: FooterColumn[];
	/** Bottom line, e.g. copyright. */
	note?: ReactNode;
	className?: string;
}

export function Footer({ logo, columns = [], note, className }: FooterProps) {
	return (
		<footer className={cn("border-t border-white/5 px-6 py-16", className)}>
			<div className="max-w-6xl mx-auto">
				<div className="flex flex-col sm:flex-row justify-between gap-10">
					{logo && <div className="font-mono font-bold">{logo}</div>}
					<div className="grid grid-cols-2 sm:grid-cols-4 gap-8">
						{columns.map((col) => (
							<div key={col.title}>
								<div className="text-xs font-mono uppercase tracking-wider text-text-muted mb-3">
									{col.title}
								</div>
								<ul className="space-y-2 text-sm">
									{col.links.map((l) => (
										<li key={l.href}>
											<a
												href={l.href}
												className="text-text-muted hover:text-text-primary transition-colors"
											>
												{l.label}
											</a>
										</li>
									))}
								</ul>
							</div>
						))}
					</div>
				</div>
				{note && (
					<div className="mt-12 pt-6 border-t border-white/5 text-xs text-text-muted">{note}</div>
				)}
			</div>
		</footer>
	);
}
