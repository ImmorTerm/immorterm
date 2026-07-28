import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import { mkdtempSync, rmSync, mkdirSync } from 'fs';
import { tmpdir } from 'os';

import {
  defaultVendorsConfig,
  writeProjectConfig,
  type ProjectConfig,
} from '../../../utils/immorterm-config';
import { writeAllVendorConfigs, syncCodexSkills } from '../hook-installer';

describe('Phase A T2 — vendor-router (writeAllVendorConfigs)', () => {
  let tmp: string;

  beforeEach(() => {
    tmp = mkdtempSync(path.join(tmpdir(), 'immorterm-vendor-router-'));
    // Pretend the project is a git repo for the Aider post-commit branch.
    mkdirSync(path.join(tmp, '.git'), { recursive: true });
  });

  afterEach(() => {
    rmSync(tmp, { recursive: true, force: true });
  });

  /**
   * A deliberate "user ticked the vendors they want" map.
   *
   * Defaults are opt-IN (Claude only), so tests that exercise the vendor
   * writers have to enable them explicitly. Note `gemini: false`: a map with
   * EVERY vendor true is treated by `resolveVendors` as the legacy auto-written
   * opt-out config and reset to defaults, so a fully-true seed would write
   * nothing. Gemini is the one vendor with no config writer, so switching it
   * off models a real user choice without changing what gets materialized.
   */
  function allVendorsEnabled(): ReturnType<typeof defaultVendorsConfig> {
    return {
      claudeCode: { enabled: true },
      codex: { enabled: true },
      cursor: { enabled: true },
      windsurf: { enabled: true },
      cline: { enabled: true },
      opencode: { enabled: true },
      gemini: { enabled: false },
      aider: { enabled: true },
      copilot: { enabled: true },
    };
  }

  function seedConfig(vendorsOverride: Partial<ReturnType<typeof defaultVendorsConfig>> = {}): void {
    const cfg: ProjectConfig = {
      version: 3,
      projectId: 'test-proj',
      services: {
        memory: { enabled: false, graph: false },
        mcpGateway: { enabled: false },
        vendors: { ...allVendorsEnabled(), ...vendorsOverride },
      },
    };
    writeProjectConfig(tmp, cfg);
  }

  it('materializes all 8 non-Claude vendor config files when all vendors are enabled', () => {
    seedConfig();

    const written = writeAllVendorConfigs(tmp);

    // Stub wrapper scripts seeded under .immorterm/hooks/lib/
    const libDir = path.join(tmp, '.immorterm', 'hooks', 'lib');
    expect(fs.existsSync(path.join(libDir, 'cursor-adapter.sh'))).toBe(true);
    expect(fs.existsSync(path.join(libDir, 'windsurf-adapter.sh'))).toBe(true);
    expect(fs.existsSync(path.join(libDir, 'cline-adapter.sh'))).toBe(true);
    expect(fs.existsSync(path.join(libDir, 'aider-post-commit.sh'))).toBe(true);

    // Codex
    expect(fs.existsSync(path.join(tmp, '.codex', 'hooks.json'))).toBe(true);
    // Cursor
    expect(fs.existsSync(path.join(tmp, '.cursor', 'hooks.json'))).toBe(true);
    // Windsurf
    expect(fs.existsSync(path.join(tmp, '.windsurf', 'hooks.json'))).toBe(true);
    // Cline — per-event executable trampolines
    expect(fs.existsSync(path.join(tmp, '.clinerules', 'hooks', 'TaskStart'))).toBe(true);
    expect(fs.existsSync(path.join(tmp, '.clinerules', 'hooks', 'PostToolUse'))).toBe(true);
    // Aider — appended to .git/hooks/post-commit
    expect(fs.existsSync(path.join(tmp, '.git', 'hooks', 'post-commit'))).toBe(true);
    // opencode
    expect(fs.existsSync(path.join(tmp, 'opencode.json'))).toBe(true);
    // Copilot — .github/hooks/immorterm.json
    expect(fs.existsSync(path.join(tmp, '.github', 'hooks', 'immorterm.json'))).toBe(true);

    // Returned list should include the JSON configs (sanity check).
    expect(written).toEqual(
      expect.arrayContaining([
        path.join(tmp, '.codex', 'hooks.json'),
        path.join(tmp, '.cursor', 'hooks.json'),
        path.join(tmp, '.windsurf', 'hooks.json'),
        path.join(tmp, 'opencode.json'),
        path.join(tmp, '.github', 'hooks', 'immorterm.json'),
      ])
    );
  });

  it('Copilot config has PascalCase events and Copilot-shape entries', () => {
    seedConfig();
    writeAllVendorConfigs(tmp);

    const copilotPath = path.join(tmp, '.github', 'hooks', 'immorterm.json');
    expect(fs.existsSync(copilotPath)).toBe(true);
    const cfg = JSON.parse(fs.readFileSync(copilotPath, 'utf8'));

    // Schema marker version present
    expect(cfg.version).toBe(1);

    // PascalCase event names — required so Copilot emits the
    // Claude-shape stdin envelope verbatim, letting our existing hook
    // scripts read it without re-keying.
    expect(cfg.hooks.SessionStart).toBeDefined();
    expect(cfg.hooks.Stop).toBeDefined();
    expect(cfg.hooks.PostToolUse).toBeDefined();

    // Each entry uses Copilot's flat shape ({type, bash, timeoutSec}),
    // NOT Claude's nested shape ({hooks: [{type, command}]}). If the
    // installer ever drifts back to Claude shape, Copilot silently
    // ignores the entries and digestion stops working — guard that.
    const sessionStart = cfg.hooks.SessionStart[0];
    expect(sessionStart.type).toBe('command');
    expect(typeof sessionStart.bash).toBe('string');
    expect(sessionStart.bash).toMatch(/immorterm-memory-guide\.sh$/);
    expect(typeof sessionStart.timeoutSec).toBe('number');
    expect(sessionStart).not.toHaveProperty('command'); // Claude shape would set this
    expect(sessionStart).not.toHaveProperty('hooks');   // Claude wraps in `hooks: []`

    // Bash paths point at .immorterm/hooks/ (the vendor-neutral
    // location), so all 9 vendors share the same scripts.
    expect(cfg.hooks.Stop[0].bash).toMatch(/\.immorterm\/hooks\//);
    expect(cfg.hooks.PostToolUse[0].bash).toMatch(/\.immorterm\/hooks\//);

    // ai_tool tagging — IMMORTERM_AI_TOOL=copilot must be exported before
    // the hook script runs so memory-guide.sh tags memories correctly.
    // Without this, Copilot sessions would still appear as ai_tool=
    // claude-code in memory (silent vendor mis-attribution).
    for (const event of ['SessionStart', 'Stop', 'PostToolUse']) {
      const entry = cfg.hooks[event][0];
      expect(entry.bash).toMatch(/IMMORTERM_AI_TOOL=copilot/);
    }
  });

  it('skips Copilot when copilot vendor is disabled', () => {
    seedConfig({ copilot: { enabled: false } });
    writeAllVendorConfigs(tmp);

    expect(fs.existsSync(path.join(tmp, '.github', 'hooks', 'immorterm.json'))).toBe(false);
    // Other vendors unaffected
    expect(fs.existsSync(path.join(tmp, '.codex', 'hooks.json'))).toBe(true);
    expect(fs.existsSync(path.join(tmp, '.cursor', 'hooks.json'))).toBe(true);
  });

  it('skips vendor config writes when that vendor is disabled', () => {
    seedConfig({ cursor: { enabled: false }, opencode: { enabled: false } });

    writeAllVendorConfigs(tmp);

    expect(fs.existsSync(path.join(tmp, '.cursor', 'hooks.json'))).toBe(false);
    expect(fs.existsSync(path.join(tmp, 'opencode.json'))).toBe(false);
    // Other vendors still present
    expect(fs.existsSync(path.join(tmp, '.codex', 'hooks.json'))).toBe(true);
    expect(fs.existsSync(path.join(tmp, '.windsurf', 'hooks.json'))).toBe(true);
  });

  it('idempotently rewrites our own vendor configs (marker preserved)', () => {
    seedConfig();
    writeAllVendorConfigs(tmp);
    const cursorPath = path.join(tmp, '.cursor', 'hooks.json');
    const first = fs.readFileSync(cursorPath, 'utf8');
    expect(first).toContain('_immortermManaged');

    // Second pass should not throw and should still contain the marker.
    expect(() => writeAllVendorConfigs(tmp)).not.toThrow();
    const second = fs.readFileSync(cursorPath, 'utf8');
    expect(second).toContain('_immortermManaged');
  });

  it('does NOT clobber an existing user-owned vendor config (no marker)', () => {
    seedConfig();
    const cursorPath = path.join(tmp, '.cursor', 'hooks.json');
    mkdirSync(path.dirname(cursorPath), { recursive: true });
    const userContent = JSON.stringify({ hooks: { afterFileEdit: ['user-script.sh'] } }, null, 2);
    fs.writeFileSync(cursorPath, userContent, 'utf8');

    writeAllVendorConfigs(tmp);

    const after = fs.readFileSync(cursorPath, 'utf8');
    expect(after).toBe(userContent);
    expect(after).not.toContain('_immortermManaged');
  });

  it('appends Aider block to existing post-commit, idempotently', () => {
    seedConfig({ codex: { enabled: false }, cursor: { enabled: false }, windsurf: { enabled: false }, cline: { enabled: false }, opencode: { enabled: false } });
    const postCommit = path.join(tmp, '.git', 'hooks', 'post-commit');
    mkdirSync(path.dirname(postCommit), { recursive: true });
    fs.writeFileSync(postCommit, '#!/bin/bash\n# user hook content\necho hi\n', { mode: 0o755 });

    writeAllVendorConfigs(tmp);
    const onceContent = fs.readFileSync(postCommit, 'utf8');
    expect(onceContent).toContain('# user hook content');
    expect(onceContent).toContain('# >>> immorterm');
    expect(onceContent).toContain('# <<< immorterm');

    // Second call should not duplicate the block.
    writeAllVendorConfigs(tmp);
    const twiceContent = fs.readFileSync(postCommit, 'utf8');
    const beginCount = (twiceContent.match(/# >>> immorterm/g) || []).length;
    expect(beginCount).toBe(1);
  });

  it('honours an all-enabled map once the user has been through the picker', () => {
    // Every vendor true looks exactly like the legacy auto-written opt-out
    // config, so without the vendorsChosen marker it gets reset to defaults and
    // ticking all nine silently does nothing.
    const everyVendorOn = {
      claudeCode: { enabled: true }, codex: { enabled: true }, cursor: { enabled: true },
      windsurf: { enabled: true }, cline: { enabled: true }, opencode: { enabled: true },
      gemini: { enabled: true }, aider: { enabled: true }, copilot: { enabled: true },
    };
    const write = (vendorsChosen?: boolean) => {
      const cfg: ProjectConfig = {
        version: 3,
        projectId: 'test-proj',
        services: {
          memory: { enabled: false, graph: false },
          mcpGateway: { enabled: false },
          vendors: everyVendorOn,
          ...(vendorsChosen === undefined ? {} : { vendorsChosen }),
        },
      };
      writeProjectConfig(tmp, cfg);
      writeAllVendorConfigs(tmp);
    };

    write(true);
    expect(fs.existsSync(path.join(tmp, '.codex', 'hooks.json'))).toBe(true);
    expect(fs.existsSync(path.join(tmp, '.cursor', 'hooks.json'))).toBe(true);

    // Legacy config with no marker: still reset to opt-in defaults.
    rmSync(path.join(tmp, '.codex'), { recursive: true, force: true });
    rmSync(path.join(tmp, '.cursor'), { recursive: true, force: true });
    write(undefined);
    expect(fs.existsSync(path.join(tmp, '.codex', 'hooks.json'))).toBe(false);
  });

  it('does not rewrite an unchanged vendor config (keeps Codex hook trust valid)', () => {
    // Codex fingerprints each hook and re-prompts "Hooks need review" on any
    // change, running none of them until accepted. A no-op rewrite on every
    // install would make that prompt appear every session.
    seedConfig();
    writeAllVendorConfigs(tmp);
    const codexPath = path.join(tmp, '.codex', 'hooks.json');
    const before = fs.readFileSync(codexPath, 'utf8');

    // Backdate so any rewrite is visible in the mtime.
    const stale = new Date(Date.now() - 60_000);
    fs.utimesSync(codexPath, stale, stale);
    const staleMtime = fs.statSync(codexPath).mtimeMs;

    writeAllVendorConfigs(tmp);

    expect(fs.readFileSync(codexPath, 'utf8')).toBe(before);
    expect(fs.statSync(codexPath).mtimeMs).toBe(staleMtime);
  });

  it('writes no vendor configs when project config is missing (opt-in defaults)', () => {
    // No config seeded — reader returns null, router uses defaults, and the
    // defaults enable Claude Code only. Enabling a vendor drops files into the
    // user's project root, so it stays a deliberate tick in the wizard.
    writeAllVendorConfigs(tmp);
    expect(fs.existsSync(path.join(tmp, '.codex', 'hooks.json'))).toBe(false);
    expect(fs.existsSync(path.join(tmp, '.cursor', 'hooks.json'))).toBe(false);
  });

  it('Codex config is Codex-shaped: description marker, no sibling key, wrapped env', () => {
    seedConfig();
    writeAllVendorConfigs(tmp);

    const raw = fs.readFileSync(path.join(tmp, '.codex', 'hooks.json'), 'utf8');
    const cfg = JSON.parse(raw);

    // Codex parses hooks.json with deny_unknown_fields over {description, hooks}
    // — a sibling `_immortermManaged` key makes it reject the whole file.
    expect(Object.keys(cfg).sort()).toEqual(['description', 'hooks']);
    expect(raw).not.toContain('_immortermManaged');
    expect(cfg.description).toContain('ImmorTerm');

    // Claude-shape nested entries, and every script tagged as Codex so memories
    // are attributed to the right vendor.
    for (const event of ['SessionStart', 'UserPromptSubmit', 'PostToolUse', 'Stop', 'SessionEnd']) {
      expect(Array.isArray(cfg.hooks[event])).toBe(true);
      const entries = cfg.hooks[event][0].hooks;
      expect(entries[0].type).toBe('command');
    }
    expect(cfg.hooks.SessionStart[0].hooks[0].command).toMatch(/IMMORTERM_AI_TOOL=codex/);
    expect(cfg.hooks.Stop[0].hooks[0].command).toMatch(/IMMORTERM_AI_TOOL=codex/);
    // Sidebar state: breathing dot on prompt submit, stops on Stop.
    expect(JSON.stringify(cfg.hooks.UserPromptSubmit)).toContain('immorterm-notify.mjs working');
    expect(JSON.stringify(cfg.hooks.Stop)).toContain('immorterm-notify.mjs idle');
    expect(JSON.stringify(cfg.hooks.PermissionRequest)).toContain('immorterm-notify.mjs attention');

    // Codex 0.145 parses `async` but does not implement it — it SKIPS the hook
    // and warns "async hooks are not supported yet". Shipping it silently
    // disabled code-change capture and the session-end digest.
    expect(raw).not.toContain('"async"');
    // SessionEnd is hard-clamped to 3s by Codex; asking for more just warns.
    expect(cfg.hooks.SessionEnd[0].hooks[0].timeout).toBe(3);
  });

  it('un-ticking Claude Code strips our .claude/ entries but keeps the user\'s', () => {
    // Simulate an already-installed Claude integration sitting alongside the
    // user's own hook, MCP server and permissions.
    const claudeDir = path.join(tmp, '.claude');
    mkdirSync(path.join(claudeDir, 'commands', 'immorterm'), { recursive: true });
    mkdirSync(path.join(claudeDir, 'skills', 'create-pr'), { recursive: true });
    fs.writeFileSync(path.join(claudeDir, 'commands', 'immorterm', 'recall.md'), '# recall');
    fs.writeFileSync(path.join(claudeDir, 'skills', 'create-pr', 'SKILL.md'), '# skill');
    fs.writeFileSync(
      path.join(claudeDir, 'settings.local.json'),
      JSON.stringify({
        hooks: {
          PreToolUse: [
            { matcher: 'Bash', hooks: [{ type: 'command', command: 'bash .immorterm/hooks/immorterm-x.sh' }] },
            { matcher: 'Bash', hooks: [{ type: 'command', command: 'echo mine' }] },
          ],
        },
        mcpServers: { 'immorterm-memory': { type: 'http' }, mine: { type: 'stdio' } },
        permissions: { allow: ['Bash(ls:*)'] },
      })
    );

    seedConfig({ claudeCode: { enabled: false } });
    writeAllVendorConfigs(tmp);

    const settings = JSON.parse(
      fs.readFileSync(path.join(claudeDir, 'settings.local.json'), 'utf8')
    );
    // Ours gone, theirs untouched.
    expect(JSON.stringify(settings)).not.toContain('immorterm');
    expect(settings.hooks.PreToolUse).toHaveLength(1);
    expect(settings.hooks.PreToolUse[0].hooks[0].command).toBe('echo mine');
    expect(settings.mcpServers).toEqual({ mine: { type: 'stdio' } });
    expect(settings.permissions).toEqual({ allow: ['Bash(ls:*)'] });

    // Slash commands and skills we deployed are removed.
    expect(fs.existsSync(path.join(claudeDir, 'commands'))).toBe(false);
    expect(fs.existsSync(path.join(claudeDir, 'skills'))).toBe(false);
  });

  describe('Codex skills (global, ~/.codex/skills)', () => {
    // Codex has no slash-command directory — a skill is its equivalent, matched
    // on description. Exercised against an isolated CODEX_HOME so it never
    // touches the developer's real one.
    let codexHome: string;
    let prevHome: string | undefined;

    beforeEach(() => {
      codexHome = mkdtempSync(path.join(tmpdir(), 'immorterm-codexhome-'));
      prevHome = process.env.CODEX_HOME;
      process.env.CODEX_HOME = codexHome;
    });

    afterEach(() => {
      if (prevHome === undefined) delete process.env.CODEX_HOME;
      else process.env.CODEX_HOME = prevHome;
      rmSync(codexHome, { recursive: true, force: true });
    });

    it('installs both skills with a Codex-shaped frontmatter name', () => {
      syncCodexSkills(true);

      for (const name of ['immorterm-recall', 'immorterm-ask']) {
        const file = path.join(codexHome, 'skills', name, 'SKILL.md');
        expect(fs.existsSync(file)).toBe(true);
        const body = fs.readFileSync(file, 'utf8');
        // Codex keys the skill off `name`; the description drives auto-invoke.
        expect(body.startsWith(`---\nname: ${name}\ndescription: `)).toBe(true);
        // ONE frontmatter block — the name is spliced into the existing one,
        // not prepended as a second. (The body itself contains `---` markdown
        // rules, so only the head is meaningful here.)
        const [, frontmatter] = body.split('---', 2);
        expect(frontmatter.match(/^name:/gm)).toHaveLength(1);
        expect(frontmatter.match(/^description:/gm)).toHaveLength(1);
        // Vendor-neutral wording: these recall sessions from any AI tool.
        expect(body).not.toContain('previous Claude Code session');
      }
    });

    it('un-ticking Codex removes them again', () => {
      syncCodexSkills(true);
      expect(fs.existsSync(path.join(codexHome, 'skills', 'immorterm-ask'))).toBe(true);

      syncCodexSkills(false);
      expect(fs.existsSync(path.join(codexHome, 'skills', 'immorterm-ask'))).toBe(false);
      expect(fs.existsSync(path.join(codexHome, 'skills', 'immorterm-recall'))).toBe(false);
    });

    it('rewriting identical content leaves the file untouched', () => {
      syncCodexSkills(true);
      const file = path.join(codexHome, 'skills', 'immorterm-recall', 'SKILL.md');
      const before = fs.statSync(file).mtimeMs;

      syncCodexSkills(true);
      expect(fs.statSync(file).mtimeMs).toBe(before);
    });
  });
});
