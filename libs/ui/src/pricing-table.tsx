import { FREE_LIMITS } from "@immorterm/tier-config";
import { Check, Clock } from "lucide-react";
import { Card } from "./card";
import { cn } from "./cn";

export interface PricingFeature {
	label: string;
	/** Not shipped yet — rendered with a "coming soon" marker. */
	soon?: boolean;
}

export interface PricingTier {
	name: string;
	/** Display price, e.g. "$0" or "$29". */
	price: string;
	period?: string;
	tagline?: string;
	features: PricingFeature[];
	cta?: { label: string; href: string };
	highlighted?: boolean;
	/** Mascot mini perched on the card corner (image src, ~80px, hidden <520px). */
	sticker?: string;
}

/**
 * Free-tier limit lines derived from libs/tier-config/config.json —
 * the single source of truth also consumed by the memory service.
 */
export function freeTierLimitFeatures(): PricingFeature[] {
	return [
		{ label: `${FREE_LIMITS.memorySearchResults} memory search results per query` },
		{ label: `${FREE_LIMITS.memoryRetentionHours}h memory retention` },
	];
}

export interface PricingTableProps {
	tiers: PricingTier[];
	className?: string;
}

export function PricingTable({ tiers, className }: PricingTableProps) {
	return (
		<div
			className={cn(
				"grid gap-5",
				tiers.length >= 3 ? "sm:grid-cols-3" : "sm:grid-cols-2",
				"max-w-4xl mx-auto",
				className,
			)}
		>
			{tiers.map((tier) => (
				<Card
					key={tier.name}
					className={cn(
						"relative flex flex-col",
						tier.highlighted &&
							"border-2 border-[var(--brand-ink)] shadow-[6px_6px_0_0_var(--brand-ink)]",
					)}
				>
					{tier.sticker && (
						<img
							src={tier.sticker}
							alt=""
							aria-hidden="true"
							width={80}
							height={80}
							className="pointer-events-none absolute -top-9 right-4 hidden h-20 w-20 rotate-[5deg] min-[520px]:block"
						/>
					)}
					<div className="flex items-center justify-between">
						<div className="font-mono text-sm uppercase tracking-wider text-text-muted">
							{tier.name}
						</div>
						{tier.highlighted && (
							<span className="rounded border border-brand/30 bg-brand/15 px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wider text-brand">
								popular
							</span>
						)}
					</div>
					<div className="mt-3 flex items-baseline gap-1">
						<span className="text-4xl font-extrabold tracking-tight">{tier.price}</span>
						{tier.period && <span className="text-sm text-text-muted">{tier.period}</span>}
					</div>
					{tier.tagline && <p className="mt-2 text-sm text-text-muted">{tier.tagline}</p>}
					<ul className="mt-6 flex-1 space-y-2.5 text-sm">
						{tier.features.map((f) => (
							<li key={f.label} className="flex items-start gap-2">
								{f.soon ? (
									<Clock className="mt-0.5 h-4 w-4 shrink-0 text-warning" />
								) : (
									<Check className="mt-0.5 h-4 w-4 shrink-0 text-success" />
								)}
								<span className={cn("text-text-muted", f.soon && "italic")}>
									{f.label}
									{f.soon && (
										<span className="ml-1.5 font-mono text-[10px] uppercase tracking-wider text-warning">
											coming soon
										</span>
									)}
								</span>
							</li>
						))}
					</ul>
					{tier.cta && (
						<a
							href={tier.cta.href}
							className={cn(
								"mt-6 inline-flex items-center justify-center rounded-xl px-5 py-2.5 text-sm font-semibold transition-all magnetic-btn",
								tier.highlighted
									? "bg-brand text-white hover:bg-brand-dark"
									: "border border-white/15 text-text-primary hover:border-brand/50",
							)}
						>
							{tier.cta.label}
						</a>
					)}
				</Card>
			))}
		</div>
	);
}
