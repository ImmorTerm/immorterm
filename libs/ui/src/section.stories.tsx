import type { Meta, StoryObj } from "@storybook/react-vite";
import { Card } from "./card";
import { Section } from "./section";

const meta = {
	title: "UI/Section",
	component: Section,
	parameters: { layout: "fullscreen" },
	args: {
		eyebrow: "memory",
		title: "nothing forgotten",
		subtitle: "Every session, decision, and code change lands in searchable memory.",
		children: (
			<Card className="mx-auto max-w-md text-sm text-text-muted">Section content goes here.</Card>
		),
	},
} satisfies Meta<typeof Section>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Default: Story = {};

export const NoDivider: Story = {
	args: { divider: false },
};

export const ChildrenOnly: Story = {
	args: { eyebrow: undefined, title: undefined, subtitle: undefined },
};
