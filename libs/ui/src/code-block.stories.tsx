import type { Meta, StoryObj } from "@storybook/react-vite";
import { CodeBlock } from "./code-block";

const meta = {
	title: "UI/CodeBlock",
	component: CodeBlock,
	args: {
		lines: ["npx immorterm"],
		label: "Install",
	},
} satisfies Meta<typeof CodeBlock>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Install: Story = {};

export const MultiLine: Story = {
	args: {
		label: "Quick start",
		lines: ["npx immorterm", "immorterm -ls", "immorterm -r main"],
	},
};

export const NoLabel: Story = {
	args: { label: undefined },
};
