#!/usr/bin/env bun
// Production-image size gate. Founder rule (same as FLAM): every image that
// ships to a production registry is <= 300 MB. Image size IS deploy latency,
// and deploy latency is how long a bad deploy stays live.
//
// This repo ships NO production Docker image today — ImmorTerm's products are
// the Tauri desktop app and the npm packages, neither a container. The one
// Dockerfile that exists (immorterm-ai:headless) is a batteries-included AGENT
// SANDBOX: a stand-in VPS a founder SSHes into to type `claude` and get an
// agent loop. It carries nodejs + @anthropic-ai/claude-code + ssh + editors on
// purpose, so it is exempt BY NAME below — not silently, but as a reviewable
// line with a reason. Add a real deployable service image here with gated:true
// and the 300 MB ceiling is enforced on it.
//
// Usage:
//   bun scripts/check-image-size.ts            # check every declared image, current tag
//   bun scripts/check-image-size.ts <ref>      # also measure this exact built ref
//   IMG=immorterm-ai:headless-latest bun scripts/check-image-size.ts $IMG

const LIMIT_MB = 300;

type Image = { ref: string; gated: boolean; why: string };

const registry: Image[] = [
  {
    ref: "immorterm-ai:headless-latest",
    gated: false,
    why: "agent sandbox (nodejs + claude-code + ssh), not a production service — batteries-included by design",
  },
];

async function sizeMb(ref: string): Promise<number | null> {
  const proc = Bun.spawn(["docker", "images", ref, "--format", "{{.Size}}"], {
    stdout: "pipe",
    stderr: "pipe",
  });
  const out = (await new Response(proc.stdout).text()).trim().split("\n")[0] ?? "";
  const m = /^([\d.]+)\s*([KMG]B)$/i.exec(out);
  if (!m) return null;
  const n = Number(m[1]);
  const unit = m[2]!.toUpperCase();
  return unit === "GB" ? n * 1024 : unit === "KB" ? n / 1024 : n;
}

// An extra ref passed on argv (e.g. the ref the build script just built) is
// matched against the registry so its gate flag applies; unknown refs are
// measured and gated by default (a new production image must be declared).
const extra = process.argv.slice(2).find((a) => !a.startsWith("-"));
const images = [...registry];
if (extra && !images.some((i) => i.ref === extra)) {
  images.push({ ref: extra, gated: true, why: "undeclared image — declare it in scripts/check-image-size.ts" });
}

let failed = false;
for (const img of images) {
  const mb = await sizeMb(img.ref);
  if (mb === null) {
    console.log(`${img.ref}: not built locally — skipped`);
    continue;
  }
  const tag = img.gated ? "GATED" : "exempt";
  console.log(`${img.ref}: ${mb.toFixed(0)} MB  [${tag}] ${img.gated ? "" : "— " + img.why}`);
  if (img.gated && mb > LIMIT_MB) {
    console.error(`  FAIL: ${img.ref} is ${mb.toFixed(0)} MB, over the ${LIMIT_MB} MB ceiling.`);
    failed = true;
  }
}

if (failed) {
  console.error(
    `\nProduction images must be ${LIMIT_MB} MB or smaller. Split the runtime out of the build stage and ship only the binaries + real runtime libs.`,
  );
  process.exit(1);
}
console.log(`\nEvery gated production image is ${LIMIT_MB} MB or smaller.`);
