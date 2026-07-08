import type { ComponentPropsWithoutRef } from "react";
import { cn } from "./cn";

export interface CardProps extends ComponentPropsWithoutRef<"div"> {
	/** Adds hover lift + brand border tint. */
	interactive?: boolean;
	/** Frosted glass background instead of solid card. */
	glass?: boolean;
}

export function Card({ interactive, glass, className, ...rest }: CardProps) {
	return (
		<div
			className={cn(
				"rounded-2xl border border-white/10 bg-bg-card p-6",
				glass && "glass-card",
				interactive && "hover-lift hover:border-brand/40",
				className,
			)}
			{...rest}
		/>
	);
}
