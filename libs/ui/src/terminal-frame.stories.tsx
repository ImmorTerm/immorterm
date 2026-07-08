import type { Meta, StoryObj } from "@storybook/react-vite";
import { TerminalFrame } from "./terminal-frame";

const CONTENT = (
	<>
		<div>$ immorterm -ls</div>
		<div className="text-text-muted">main · zsh · alive 42d</div>
		<div className="text-text-muted">claude · alive 3h</div>
		<div>
			$ <span className="animate-pulse">▍</span>
		</div>
	</>
);

const meta = {
	title: "UI/TerminalFrame",
	component: TerminalFrame,
	args: {
		title: "immorterm — zsh",
		children: CONTENT,
	},
} satisfies Meta<typeof TerminalFrame>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Default: Story = {};

export const NoGlow: Story = {
	args: { glow: false },
};

export const NoTitle: Story = {
	args: { title: undefined },
};
