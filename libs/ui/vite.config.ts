import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// Storybook's @storybook/react-vite auto-merges this config; the Tailwind v4
// plugin is what compiles the `@import "tailwindcss"` in apps/web globals.css
// that .storybook/preview.tsx pulls in. This package ships source only.
export default defineConfig({
	plugins: [react(), tailwindcss()],
});
