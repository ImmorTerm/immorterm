import type { Meta, StoryObj } from "@storybook/react-vite";
import { Marquee } from "./marquee";

const meta = {
	title: "UI/Marquee",
	component: Marquee,
	parameters: { layout: "fullscreen" },
	args: {
		text: "sessions that refuse to die · memory that never forgets",
	},
} satisfies Meta<typeof Marquee>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Default: Story = {};
