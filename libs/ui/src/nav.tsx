import { Menu, X } from "lucide-react";
import type { ReactNode } from "react";
import { cn } from "./cn";

export interface NavLink {
	label: string;
	href: string;
	/** Highlight with the theme accent (e.g. the /ai item). */
	accent?: boolean;
}

export interface NavProps {
	logo: ReactNode;
	links?: NavLink[];
	/** Right-side call to action (Button, ThemeSwitcher, etc.). */
	cta?: ReactNode;
	className?: string;
}

function linkClasses(accent?: boolean) {
	return cn(
		"transition-colors",
		accent ? "text-accent hover:text-text-primary" : "text-text-muted hover:text-text-primary",
	);
}

export function Nav({ logo, links = [], cta, className }: NavProps) {
	return (
		<header
			className={cn(
				"sticky top-0 z-50 border-b border-white/5 bg-bg/80 backdrop-blur-md",
				className,
			)}
		>
			<nav className="max-w-6xl mx-auto flex items-center justify-between gap-6 px-6 h-16">
				<div className="flex items-center gap-2 font-mono font-bold">{logo}</div>
				<div className="hidden sm:flex items-center gap-6 text-sm">
					{links.map((l) => (
						<a key={l.href} href={l.href} className={linkClasses(l.accent)}>
							{l.label}
						</a>
					))}
				</div>
				<div className="flex items-center gap-3">
					{cta}
					{links.length > 0 && (
						/* ponytail: native <details> hamburger — zero client JS; menu stays
						   open after an in-page anchor click. Upgrade to a client component
						   with onClick-close if that ever annoys anyone. */
						<details className="group relative sm:hidden">
							<summary
								className="flex h-9 w-9 cursor-pointer list-none items-center justify-center rounded-lg border border-white/10 text-text-muted transition-colors hover:text-text-primary [&::-webkit-details-marker]:hidden"
								aria-label="Menu"
							>
								<Menu className="h-4 w-4 group-open:hidden" />
								<X className="hidden h-4 w-4 group-open:block" />
							</summary>
							<div className="absolute right-0 top-11 flex min-w-44 flex-col gap-1 rounded-xl border border-white/10 bg-bg-elevated p-2 text-sm shadow-lg shadow-black/40">
								{links.map((l) => (
									<a
										key={l.href}
										href={l.href}
										className={cn("rounded-lg px-3 py-2 hover:bg-white/5", linkClasses(l.accent))}
									>
										{l.label}
									</a>
								))}
							</div>
						</details>
					)}
				</div>
			</nav>
		</header>
	);
}
