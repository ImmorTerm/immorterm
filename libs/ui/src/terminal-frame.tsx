import type { ReactNode } from "react";
import { cn } from "./cn";

export interface TerminalFrameProps {
	/** Title shown in the chrome bar, e.g. "immorterm — zsh". */
	title?: string;
	children: ReactNode;
	className?: string;
	/** Disable the theme-accent glow. */
	glow?: boolean;
}

/** Aquarium-glass terminal chrome (design-lock v2 §6) for screenshots and demos. */
export function TerminalFrame({ title, children, className, glow = true }: TerminalFrameProps) {
	return (
		<div
			className={cn(
				"tank-glass overflow-hidden text-left text-[var(--brand-foam)]",
				glow && "ui-terminal-glow",
				className,
			)}
		>
			<div className="flex items-center gap-2 border-b border-white/10 bg-white/5 px-4 py-2.5">
				<span className="h-3 w-3 rounded-full bg-danger/80" />
				<span className="h-3 w-3 rounded-full bg-warning/80" />
				<span className="h-3 w-3 rounded-full bg-success/80" />
				{title && <span className="ml-3 truncate font-mono text-xs text-text-muted">{title}</span>}
			</div>
			<div className="p-5 font-mono text-sm leading-relaxed">{children}</div>
		</div>
	);
}
