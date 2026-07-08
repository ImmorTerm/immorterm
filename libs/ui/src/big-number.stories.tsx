import type { Meta, StoryObj } from "@storybook/react-vite";
import { BigNumber } from "./big-number";

const meta = {
	title: "UI/BigNumber",
	component: BigNumber,
	args: {
		value: "0",
		caption: "sessions lost since install",
	},
} satisfies Meta<typeof BigNumber>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Default: Story = {};

export const WithChildren: Story = {
	args: {
		value: "42d",
		caption: "longest session uptime",
		children: (
			<p className="mt-4 text-sm text-text-muted">Measured across every crash and reload.</p>
		),
	},
};
