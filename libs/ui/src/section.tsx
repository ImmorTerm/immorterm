import type { ReactNode } from "react";
import { cn } from "./cn";

export interface SectionProps {
	id?: string;
	/** Small mono label above the title. */
	eyebrow?: string;
	title?: ReactNode;
	subtitle?: ReactNode;
	children?: ReactNode;
	className?: string;
	/** Adds the top gradient divider glow. */
	divider?: boolean;
}

export function Section({
	id,
	eyebrow,
	title,
	subtitle,
	children,
	className,
	divider = true,
}: SectionProps) {
	return (
		<section
			id={id}
			className={cn(
				"relative px-6 py-24 sm:py-32",
				divider && "border-t border-white/5 section-glow",
				className,
			)}
		>
			<div className="max-w-6xl mx-auto">
				{(eyebrow || title || subtitle) && (
					<div className="text-center mb-14">
						{eyebrow && (
							<div className="text-xs font-mono uppercase tracking-[0.2em] text-text-muted mb-4">
								{eyebrow}
							</div>
						)}
						{title && (
							<h2
								className="lowercase tracking-tight"
								style={{
									fontFamily: "var(--font-display, inherit)",
									fontWeight: 800,
									fontSize: "clamp(2.5rem, 6vw, 4.5rem)",
									lineHeight: 0.95,
								}}
							>
								{title}
							</h2>
						)}
						{subtitle && <p className="mt-5 text-text-muted max-w-2xl mx-auto">{subtitle}</p>}
					</div>
				)}
				{children}
			</div>
		</section>
	);
}
