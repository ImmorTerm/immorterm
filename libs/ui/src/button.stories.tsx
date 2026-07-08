import type { Meta, StoryObj } from "@storybook/react-vite";
import { Button } from "./button";

const meta = {
	title: "UI/Button",
	component: Button,
	args: { children: "Install ImmorTerm" },
	argTypes: {
		variant: { control: "select", options: ["primary", "secondary", "ghost"] },
		size: { control: "select", options: ["sm", "md", "lg"] },
	},
} satisfies Meta<typeof Button>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Primary: Story = {};

export const Secondary: Story = {
	args: { variant: "secondary", children: "View docs" },
};

export const Ghost: Story = {
	args: { variant: "ghost", children: "Skip for now" },
};

export const Sizes: Story = {
	render: () => (
		<div className="flex items-center gap-4">
			<Button size="sm">Small</Button>
			<Button size="md">Medium</Button>
			<Button size="lg">Large</Button>
		</div>
	),
};

export const AsLink: Story = {
	args: { href: "#pricing", children: "See pricing" },
};
