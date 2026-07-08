import type { Meta, StoryObj } from "@storybook/react-vite";
import { freeTierLimitFeatures, PricingTable } from "./pricing-table";

const meta = {
	title: "UI/PricingTable",
	component: PricingTable,
	args: {
		tiers: [
			{
				name: "Free",
				price: "$0",
				tagline: "Persistent terminals, forever.",
				features: [
					{ label: "Unlimited persistent sessions" },
					...freeTierLimitFeatures(),
					{ label: "Cross-machine handoff", soon: true },
				],
				cta: { label: "Install free", href: "#install" },
			},
			{
				name: "Pro",
				price: "$12",
				period: "/mo",
				tagline: "Full memory, no limits.",
				features: [
					{ label: "Everything in Free" },
					{ label: "Unlimited memory retention" },
					{ label: "Knowledge packs" },
					{ label: "Team sharing", soon: true },
				],
				cta: { label: "Go Pro", href: "#pro" },
				highlighted: true,
			},
		],
	},
} satisfies Meta<typeof PricingTable>;

export default meta;
type Story = StoryObj<typeof meta>;

export const TwoTiers: Story = {};

export const ThreeTiers: Story = {
	args: {
		tiers: [
			...meta.args.tiers,
			{
				name: "Team",
				price: "$49",
				period: "/mo",
				tagline: "Shared memory across the whole team.",
				features: [{ label: "Everything in Pro" }, { label: "Shared project memory" }],
				cta: { label: "Contact us", href: "#team" },
			},
		],
	},
};
