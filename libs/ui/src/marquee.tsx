import { cn } from "./cn";

export interface MarqueeProps {
	/** The repeating line, `·`-separated, lowercase. Trailing separator added automatically. */
	text: string;
	className?: string;
}

/**
 * Marquee band (design-lock v2 §6) — full-bleed coral strip, Unbounded 800,
 * drifting at 75s/loop, pausable on hover. Max one per page.
 */
export function Marquee({ text, className }: MarqueeProps) {
	const line = `${text.trim().replace(/·\s*$/, "").trim()} · `;
	return (
		<div
			aria-hidden="true"
			className={cn("tank-marquee overflow-hidden py-4 sm:py-5", className)}
			style={{
				background: "var(--brand-coral)",
				borderBlock: "3px solid var(--brand-ink)",
				color: "var(--brand-ink)",
			}}
		>
			<div className="tank-marquee-track flex w-max whitespace-nowrap">
				{/* two copies so the -50% translate loops seamlessly */}
				{[0, 1].map((i) => (
					<span
						key={i}
						className="lowercase"
						style={{
							fontFamily: "var(--font-display, inherit)",
							fontWeight: 800,
							/* a drifting accent strip, not a second hero */
							fontSize: "clamp(1.125rem, 2vw, 1.5rem)",
							lineHeight: 1.1,
						}}
					>
						{line.repeat(3)}
					</span>
				))}
			</div>
		</div>
	);
}
