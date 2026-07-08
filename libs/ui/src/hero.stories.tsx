import type { Meta, StoryObj } from "@storybook/react-vite";
import { Badge } from "./badge";
import { Button } from "./button";
import { Hero } from "./hero";
import { TerminalFrame } from "./terminal-frame";

const meta = {
	title: "UI/Hero",
	component: Hero,
	parameters: { layout: "fullscreen" },
	args: {
		eyebrow: <Badge variant="accent">open beta</Badge>,
		title: (
			<>
				Terminals that <span className="text-brand">refuse to die</span>
			</>
		),
		subtitle:
			"VS Code crashed? Your sessions didn't. ImmorTerm keeps every terminal alive and reconnects automatically.",
		actions: (
			<>
				<Button size="lg">Install free</Button>
				<Button variant="secondary" size="lg">
					View docs
				</Button>
			</>
		),
	},
} satisfies Meta<typeof Hero>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Default: Story = {};

export const WithTerminalDemo: Story = {
	args: {
		children: (
			<TerminalFrame title="immorterm — zsh">
				<div>$ immorterm -ls</div>
				<div className="text-text-muted">3 sessions alive · 0 lost · uptime 42d</div>
			</TerminalFrame>
		),
	},
};
