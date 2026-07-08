import type { Meta, StoryObj } from "@storybook/react-vite";
import { Badge } from "./badge";

const meta = {
	title: "UI/Badge",
	component: Badge,
	args: { children: "beta" },
	argTypes: {
		variant: { control: "select", options: ["accent", "outline", "success", "soon"] },
	},
} satisfies Meta<typeof Badge>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Accent: Story = {};

export const AllVariants: Story = {
	render: () => (
		<div className="flex items-center gap-3">
			<Badge variant="accent">accent</Badge>
			<Badge variant="outline">outline</Badge>
			<Badge variant="success">alive</Badge>
			<Badge variant="soon">coming soon</Badge>
		</div>
	),
};
