import type { ReactNode } from "react";
import { Card } from "./card";
import { cn } from "./cn";

export interface FeatureItem {
	icon?: ReactNode;
	title: string;
	body: ReactNode;
	/** Mark features that are not shipped yet. */
	soon?: boolean;
}

export interface FeatureGridProps {
	items: FeatureItem[];
	columns?: 2 | 3 | 4;
	className?: string;
}

const COLS = {
	2: "sm:grid-cols-2",
	3: "sm:grid-cols-3",
	4: "sm:grid-cols-2 lg:grid-cols-4",
} as const;

export function FeatureGrid({ items, columns = 3, className }: FeatureGridProps) {
	return (
		<div className={cn("grid gap-4", COLS[columns], className)}>
			{items.map((item) => (
				<Card key={item.title} interactive className="p-5">
					{item.icon && <div className="mb-3 text-brand">{item.icon}</div>}
					<div className="flex items-center gap-2 text-sm font-semibold text-text-primary">
						{item.title}
						{item.soon && (
							<span className="rounded border border-warning/30 bg-warning/10 px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wider text-warning">
								coming soon
							</span>
						)}
					</div>
					<div className="mt-1.5 text-xs leading-relaxed text-text-muted">{item.body}</div>
				</Card>
			))}
		</div>
	);
}
