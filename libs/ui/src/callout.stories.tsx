import type { Meta, StoryObj } from "@storybook/react-vite";
import { Callout } from "./callout";

const meta = {
	title: "UI/Callout",
	component: Callout,
	args: {
		title: "Heads up",
		children: "The memory service keeps every session searchable, even after a crash.",
	},
	argTypes: {
		variant: { control: "select", options: ["info", "warning", "success", "soon"] },
	},
} satisfies Meta<typeof Callout>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Info: Story = {};

export const Warning: Story = {
	args: { variant: "warning", title: "Careful", children: "This kills the daemon for real." },
};

export const Success: Story = {
	args: { variant: "success", title: "Reconnected", children: "All 15 sessions came back." },
};

export const Soon: Story = {
	args: { variant: "soon", title: "Coming soon", children: "Cross-machine session handoff." },
};

export const NoTitle: Story = {
	args: { title: undefined },
};
