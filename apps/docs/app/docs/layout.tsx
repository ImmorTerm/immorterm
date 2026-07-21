import { Nav } from "@immorterm/ui";
import { DocsLayout } from "fumadocs-ui/layouts/docs";
import type { ReactNode } from "react";
import { source } from "@/lib/source";
import { BrandMark } from "../components/brand-mark";

const NAV_LINKS = [
	{ label: "Docs", href: "/docs" },
	{ label: "immorterm.com", href: "https://immorterm.com" },
	{ label: "GitHub", href: "https://github.com/ImmorTerm/immorterm" },
];

export default function Layout({ children }: { children: ReactNode }) {
	return (
		<DocsLayout
			tree={source.pageTree}
			// Replace fumadocs' navbar with the branded @immorterm/ui Nav (sticky, h-16 —
			// matches --fd-header-height in global.css). Search stays on ⌘K + the sidebar.
			nav={{ component: <Nav logo={<BrandMark />} links={NAV_LINKS} /> }}
			// fumadocs 16 dropped the `disableThemeSwitch` prop; the tank is dark-only.
			themeSwitch={{ enabled: false }}
		>
			{children}
		</DocsLayout>
	);
}
