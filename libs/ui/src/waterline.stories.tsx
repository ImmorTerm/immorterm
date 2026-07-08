import type { Meta, StoryObj } from "@storybook/react-vite";
import { Waterline } from "./waterline";

const meta = {
	title: "UI/Waterline",
	component: Waterline,
	parameters: { layout: "fullscreen" },
	args: { fill: "var(--brand-lagoon)" },
	argTypes: {
		variant: { control: "select", options: ["wave", "meniscus"] },
	},
	// Show the divider doing its job: current band above, `fill` band below.
	decorators: [
		(Story) => (
			<div>
				<div className="band-depths h-24" />
				<Story />
				<div className="h-24" style={{ background: "var(--brand-lagoon)" }} />
			</div>
		),
	],
} satisfies Meta<typeof Waterline>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Wave: Story = {};

export const Meniscus: Story = {
	args: { variant: "meniscus" },
};
