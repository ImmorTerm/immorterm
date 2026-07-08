import type { ReactNode } from "react";
import { cn } from "./cn";

export interface HeroProps {
	eyebrow?: ReactNode;
	title: ReactNode;
	subtitle?: ReactNode;
	/** Buttons / links row under the subtitle. */
	actions?: ReactNode;
	/** Below the fold content, e.g. a TerminalFrame demo. */
	children?: ReactNode;
	className?: string;
}

export function Hero({ eyebrow, title, subtitle, actions, children, className }: HeroProps) {
	return (
		<section className={cn("relative px-6 pt-24 pb-16 sm:pt-32 sm:pb-24 text-center", className)}>
			<div className="max-w-4xl mx-auto">
				{eyebrow && (
					<div className="mb-6 flex justify-center text-xs font-mono uppercase tracking-[0.2em] text-text-muted">
						{eyebrow}
					</div>
				)}
				<h1 className="text-4xl sm:text-6xl font-extrabold tracking-tight leading-[1.1]">
					{title}
				</h1>
				{subtitle && <p className="mt-6 text-lg text-text-muted max-w-2xl mx-auto">{subtitle}</p>}
				{actions && (
					<div className="mt-9 flex flex-wrap items-center justify-center gap-4">{actions}</div>
				)}
			</div>
			{children && <div className="mt-16 max-w-5xl mx-auto">{children}</div>}
		</section>
	);
}
