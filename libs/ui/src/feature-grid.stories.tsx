import type { Meta, StoryObj } from "@storybook/react-vite";
import { Brain, RefreshCw, Terminal, Zap } from "lucide-react";
import { FeatureGrid } from "./feature-grid";

const ITEMS = [
	{
		icon: <Terminal className="h-5 w-5" />,
		title: "Persistent sessions",
		body: "Terminals survive VS Code crashes, reloads, and updates.",
	},
	{
		icon: <RefreshCw className="h-5 w-5" />,
		title: "Auto-reconnect",
		body: "Reopen the editor and every session is exactly where you left it.",
	},
	{
		icon: <Brain className="h-5 w-5" />,
		title: "AI memory",
		body: "Every fact, decision, and code change is saved and searchable.",
	},
	{
		icon: <Zap className="h-5 w-5" />,
		title: "GPU rendering",
		body: "WebGPU terminal with AI overlays drawn straight on the grid.",
		soon: true,
	},
];

const meta = {
	title: "UI/FeatureGrid",
	component: FeatureGrid,
	args: { items: ITEMS.slice(0, 3) },
	argTypes: {
		columns: { control: "select", options: [2, 3, 4] },
	},
} satisfies Meta<typeof FeatureGrid>;

export default meta;
type Story = StoryObj<typeof meta>;

export const ThreeColumns: Story = {};

export const TwoColumns: Story = {
	args: { items: ITEMS.slice(0, 2), columns: 2 },
};

export const FourColumnsWithSoon: Story = {
	args: { items: ITEMS, columns: 4 },
};
