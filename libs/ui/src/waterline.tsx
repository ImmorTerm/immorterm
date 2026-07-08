import { cn } from "./cn";

export interface WaterlineProps {
	/** CSS color of the NEXT layer (the band below the divider), e.g. "var(--brand-lagoon)". */
	fill: string;
	/** "wave" (default) or the quiet "meniscus" break (2px aqua rule + bubbles). */
	variant?: "wave" | "meniscus";
	className?: string;
}

/**
 * Waterline divider (design-lock v2 §6) — every layer change gets one.
 * Place at the bottom of the current band; `fill` is the next band's color.
 */
export function Waterline({ fill, variant = "wave", className }: WaterlineProps) {
	if (variant === "meniscus") {
		return (
			<div aria-hidden="true" className={cn("relative", className)}>
				<div className="h-[2px] w-full" style={{ background: "var(--brand-aqua)" }} />
				<div className="absolute -top-2 left-1/2 flex -translate-x-1/2 gap-3">
					{[4, 6, 4].map((size, i) => (
						<span
							// biome-ignore lint/suspicious/noArrayIndexKey: static decorative dots
							key={i}
							className="rounded-full"
							style={{ width: size, height: size, background: "var(--brand-aqua)" }}
						/>
					))}
				</div>
			</div>
		);
	}
	return (
		<svg
			viewBox="0 0 1440 64"
			preserveAspectRatio="none"
			aria-hidden="true"
			className={cn("block h-10 w-full sm:h-16", className)}
			style={{ color: fill }}
		>
			<path
				fill="currentColor"
				d="M0,32 C240,56 480,8 720,32 C960,56 1200,8 1440,32 L1440,64 L0,64 Z"
			/>
		</svg>
	);
}
