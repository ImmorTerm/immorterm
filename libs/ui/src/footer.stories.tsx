import type { Meta, StoryObj } from "@storybook/react-vite";
import { Footer } from "./footer";

const meta = {
	title: "UI/Footer",
	component: Footer,
	parameters: { layout: "fullscreen" },
	args: {
		logo: <span className="text-brand">immorterm</span>,
		columns: [
			{
				title: "Product",
				links: [
					{ label: "Features", href: "#features" },
					{ label: "Pricing", href: "#pricing" },
					{ label: "Changelog", href: "#changelog" },
				],
			},
			{
				title: "Docs",
				links: [
					{ label: "Getting started", href: "#start" },
					{ label: "Memory", href: "#memory" },
				],
			},
			{
				title: "Company",
				links: [
					{ label: "About", href: "#about" },
					{ label: "Contact", href: "#contact" },
				],
			},
		],
		note: <>© 2026 ImmorTerm. Sessions that refuse to die.</>,
	},
} satisfies Meta<typeof Footer>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Default: Story = {};

export const LogoAndNoteOnly: Story = {
	args: { columns: [] },
};
