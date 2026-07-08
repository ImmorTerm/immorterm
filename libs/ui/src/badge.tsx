import type { ComponentPropsWithoutRef } from "react";
import { cn } from "./cn";

const VARIANTS = {
	accent: "bg-brand/15 text-brand border border-brand/30",
	outline: "border border-white/15 text-text-muted",
	success: "bg-success/15 text-success border border-success/30",
	/** For features that are not shipped yet. */
	soon: "bg-warning/10 text-warning border border-warning/30",
} as const;

export interface BadgeProps extends ComponentPropsWithoutRef<"span"> {
	variant?: keyof typeof VARIANTS;
}

export function Badge({ variant = "accent", className, ...rest }: BadgeProps) {
	return (
		<span
			className={cn(
				"inline-flex items-center gap-1 rounded px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wider",
				VARIANTS[variant],
				className,
			)}
			{...rest}
		/>
	);
}
