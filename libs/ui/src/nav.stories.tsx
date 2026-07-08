import type { Meta, StoryObj } from "@storybook/react-vite";
import { Button } from "./button";
import { Nav } from "./nav";

const meta = {
	title: "UI/Nav",
	component: Nav,
	parameters: { layout: "fullscreen" },
	args: {
		logo: <span className="text-brand">immorterm</span>,
		links: [
			{ label: "Features", href: "#features" },
			{ label: "Pricing", href: "#pricing" },
			{ label: "Docs", href: "#docs" },
			{ label: "AI", href: "#ai", accent: true },
		],
		cta: <Button size="sm">Install</Button>,
	},
} satisfies Meta<typeof Nav>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Default: Story = {};

export const LogoOnly: Story = {
	args: { links: [], cta: undefined },
};
