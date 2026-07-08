/**
 * @immorterm/ui — the ImmorTerm design system.
 *
 * Terminal-dark React components + design tokens derived from the
 * product's own theme palettes (libs/menu-data THEME_DEFS).
 * Styling is Tailwind v4 utility classes against the tokens defined in
 * apps/web/app/globals.css @theme plus ./styles.css theme variables —
 * consumers must include both and add this package to their Tailwind
 * `@source` and Next `transpilePackages`.
 */

export { Badge, type BadgeProps } from "./badge";
export { BigNumber, type BigNumberProps } from "./big-number";
export { Button, type ButtonProps } from "./button";
export { Callout, type CalloutProps } from "./callout";
export { Card, type CardProps } from "./card";
export { cn } from "./cn";
export { CodeBlock, CodeBlockIcon, type CodeBlockProps } from "./code-block";
export {
	FeatureGrid,
	type FeatureGridProps,
	type FeatureItem,
} from "./feature-grid";
export { Footer, type FooterColumn, type FooterProps } from "./footer";
export { Hero, type HeroProps } from "./hero";
export { Marquee, type MarqueeProps } from "./marquee";
export { Nav, type NavLink, type NavProps } from "./nav";
export {
	freeTierLimitFeatures,
	type PricingFeature,
	PricingTable,
	type PricingTableProps,
	type PricingTier,
} from "./pricing-table";
export { Section, type SectionProps } from "./section";
export { TerminalFrame, type TerminalFrameProps } from "./terminal-frame";
export {
	brand,
	DEFAULT_THEME,
	easing,
	fonts,
	mort,
	motion,
	radii,
	spacing,
	THEME_DEFS,
	type ThemeDef,
	themeCssVars,
	typeScale,
} from "./tokens";
export { Waterline, type WaterlineProps } from "./waterline";
