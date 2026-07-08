import type { ComponentPropsWithoutRef } from "react";
import { cn } from "./cn";

const VARIANTS = {
	primary: "bg-brand text-white hover:bg-brand-dark shadow-[0_0_24px_var(--theme-glow)]",
	secondary: "border border-white/15 bg-bg-elevated text-text-primary hover:border-brand/50",
	ghost: "text-text-muted hover:text-text-primary",
} as const;

const SIZES = {
	sm: "text-xs px-3 py-1.5 rounded-lg",
	md: "text-sm px-5 py-2.5 rounded-xl",
	lg: "text-base px-7 py-3.5 rounded-2xl",
} as const;

export interface ButtonProps extends ComponentPropsWithoutRef<"button"> {
	variant?: keyof typeof VARIANTS;
	size?: keyof typeof SIZES;
	/** Render as an anchor instead of a button. */
	href?: string;
}

export function Button({
	variant = "primary",
	size = "md",
	href,
	className,
	children,
	...rest
}: ButtonProps) {
	const classes = cn(
		"inline-flex items-center justify-center gap-2 font-semibold transition-all magnetic-btn",
		"focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-brand",
		VARIANTS[variant],
		SIZES[size],
		className,
	);
	if (href) {
		return (
			<a href={href} className={classes}>
				{children}
			</a>
		);
	}
	return (
		<button type="button" className={classes} {...rest}>
			{children}
		</button>
	);
}
