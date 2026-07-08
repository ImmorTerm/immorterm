import type { Preview } from "@storybook/react-vite";

// The real habitat: apps/web's Tailwind entry. Pulls in tailwindcss, the
// terminal-noir @theme tokens (dark bg + coral), the @source scan over
// libs/ui/src, and libs/ui/src/styles.css theme variables + helpers
// (magnetic-btn, glass-card, tank-glass, section-glow, …).
import "../../../apps/web/app/globals.css";

const preview: Preview = {
	parameters: {
		layout: "centered",
		controls: {
			matchers: { color: /(background|color)$/i, date: /Date$/i },
		},
		backgrounds: { disable: true },
	},
	decorators: [
		// Inline style, not utility classes: .storybook/ is outside the
		// Tailwind @source scan, so classes used only here wouldn't generate.
		(Story) => (
			<div
				style={{
					background: "var(--color-bg)",
					color: "var(--color-text-primary)",
					fontFamily: "var(--font-sans)",
				}}
			>
				<Story />
			</div>
		),
	],
};

export default preview;
