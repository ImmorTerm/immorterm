/**
 * Logo lockup for the docs Nav + Footer. Head-only Mort mark at ≤32px —
 * the six-frond crown IS the brand at this size (design-lock §7).
 * Mirrors apps/web/app/components/brand-mark.tsx.
 */
export function BrandMark() {
	return (
		<a href="/docs" className="flex items-center gap-2">
			<img
				src="/brand/mort-badge.png"
				alt="Mort"
				width={26}
				height={26}
				className="h-[26px] w-[26px]"
			/>
			<span className="text-sm font-semibold tracking-tight">
				Immor<span className="text-brand">Term</span>
				<span className="ml-2 font-mono text-xs font-normal text-text-muted">docs</span>
			</span>
		</a>
	);
}
