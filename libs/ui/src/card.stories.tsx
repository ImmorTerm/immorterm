import type { Meta, StoryObj } from "@storybook/react-vite";
import { Card } from "./card";

const meta = {
	title: "UI/Card",
	component: Card,
	args: {
		children: (
			<>
				<div className="text-sm font-semibold text-text-primary">Sessions survive</div>
				<div className="mt-1.5 text-xs leading-relaxed text-text-muted">
					VS Code crashed? Your terminal didn't. Reconnect exactly where you left off.
				</div>
			</>
		),
	},
} satisfies Meta<typeof Card>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Default: Story = {};

export const Interactive: Story = {
	args: { interactive: true },
};

export const Glass: Story = {
	args: { glass: true },
};
