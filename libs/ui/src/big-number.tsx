import type { ReactNode } from "react";
import { cn } from "./cn";

export interface BigNumberProps {
	/** The number, poster-size. Must cash against the truth ledger. */
	value: string;
	/** Mono caption under the number, lowercase. */
	caption: string;
	/** Extra content under the caption (body copy, links). */
	children?: ReactNode;
	className?: string;
}

/**
 * Oversized-number section (design-lock v2 §6) — one number, poster size,
 * one mono caption. Never more than one per band.
 */
export function BigNumber({ value, caption, children, className }: BigNumberProps) {
	return (
		<div className={cn("text-center", className)}>
			<div
				className="lowercase"
				style={{
					fontFamily: "var(--font-display, inherit)",
					fontWeight: 700,
					/* big, but never bigger than the hero — one huge statement per page */
					fontSize: "clamp(3rem, 7vw, 5.25rem)",
					lineHeight: 0.95,
					letterSpacing: "-0.02em",
				}}
			>
				{value}
			</div>
			<div className="mt-4 font-mono text-sm lowercase tracking-[0.08em] opacity-80 sm:text-base">
				{caption}
			</div>
			{children}
		</div>
	);
}
