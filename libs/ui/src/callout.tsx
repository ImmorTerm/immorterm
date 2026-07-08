import { AlertTriangle, CheckCircle2, Clock, Info } from "lucide-react";
import type { ReactNode } from "react";
import { cn } from "./cn";

const VARIANTS = {
	info: { icon: Info, classes: "border-accent/30 bg-accent/5 text-accent" },
	warning: { icon: AlertTriangle, classes: "border-warning/30 bg-warning/5 text-warning" },
	success: { icon: CheckCircle2, classes: "border-success/30 bg-success/5 text-success" },
	soon: { icon: Clock, classes: "border-brand/30 bg-brand/5 text-brand" },
} as const;

export interface CalloutProps {
	variant?: keyof typeof VARIANTS;
	title?: string;
	children: ReactNode;
	className?: string;
}

export function Callout({ variant = "info", title, children, className }: CalloutProps) {
	const { icon: Icon, classes } = VARIANTS[variant];
	return (
		<div className={cn("flex gap-3 rounded-2xl border p-4", classes, className)}>
			<Icon className="mt-0.5 h-4 w-4 shrink-0" />
			<div className="text-sm">
				{title && <div className="mb-1 font-semibold">{title}</div>}
				<div className="text-text-muted">{children}</div>
			</div>
		</div>
	);
}
