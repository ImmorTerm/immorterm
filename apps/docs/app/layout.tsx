import "./global.css";
import { Footer } from "@immorterm/ui";
import { RootProvider } from "fumadocs-ui/provider";
import type { Metadata } from "next";
import type { ReactNode } from "react";
import { BrandMark } from "./components/brand-mark";

export const metadata: Metadata = {
	title: {
		template: "%s · ImmorTerm Docs",
		default: "ImmorTerm Docs — the terminal that refuses to die",
	},
	description:
		"How to install, run, and wire up ImmorTerm: persistent sessions, local memory, MCP tools, CLI, hub, and gateway.",
	icons: { icon: "/brand/mort-badge.png" },
};

const FOOTER_COLUMNS = [
	{
		title: "Docs",
		links: [
			{ label: "Getting Started", href: "/docs" },
			{ label: "Memory API", href: "/docs/memory-api" },
			{ label: "MCP Tools", href: "/docs/mcp-tools" },
			{ label: "CLI", href: "/docs/cli" },
		],
	},
	{
		title: "Reference",
		links: [
			{ label: "Hub API", href: "/docs/hub-api" },
			{ label: "Gateway", href: "/docs/gateway" },
			{ label: "Billing API", href: "/docs/billing-api" },
		],
	},
	{
		title: "Resources",
		links: [
			{ label: "immorterm.com", href: "https://immorterm.com" },
			{ label: "GitHub", href: "https://github.com/ImmorTerm/immorterm" },
		],
	},
];

export default function RootLayout({ children }: { children: ReactNode }) {
	return (
		<html lang="en" className="dark" suppressHydrationWarning>
			<head>
				<link rel="preconnect" href="https://fonts.googleapis.com" />
				<link rel="preconnect" href="https://fonts.gstatic.com" crossOrigin="anonymous" />
				<link
					href="https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600;700;800&family=JetBrains+Mono:wght@400;500;600&display=swap"
					rel="stylesheet"
				/>
			</head>
			<body className="antialiased">
				{/* Terminal-dark only — the tank has no light mode. `type: "static"` makes the
				    search dialog fetch the prebuilt index from /api/search (Orama in-browser),
				    required for the static export. */}
				<RootProvider
					theme={{ forcedTheme: "dark", defaultTheme: "dark" }}
					search={{ options: { type: "static" } }}
				>
					{children}
					<Footer
						logo={<BrandMark />}
						columns={FOOTER_COLUMNS}
						note={
							<div className="flex flex-wrap items-center justify-between gap-3">
								<span>&copy; {new Date().getFullYear()} ImmorTerm. All rights reserved.</span>
								<span className="font-mono lowercase">the docs regrow too. — mort</span>
							</div>
						}
					/>
				</RootProvider>
			</body>
		</html>
	);
}
