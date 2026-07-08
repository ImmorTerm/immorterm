import * as vscode from 'vscode';
import * as path from 'path';
import * as fs from 'fs';
import { logger } from './logger';
import { getTheme, generateHardstatus } from '../themes';
import {
  IMMORTERM_SCRIPTS_DIR,
  IMMORTERM_GLOBAL_DIR,
  getTerminalsDir as getTerminalsDirFromConfig,
  getLogsDir as getLogsDirFromConfig,
  getPendingDir as getPendingDirFromConfig,
  getRenamesDir as getRenamesDirFromConfig,
  getGlobalScriptPath,
  getProjectScreenrcPath,
} from './immorterm-config';
import { getScrollbackBuffer } from './settings';

/**
 * Version file name - stores the extension version that last extracted resources
 * This enables force-updating resources when the extension updates
 */
const VERSION_FILE = '.immorterm-version';

/**
 * Resource names that should be extracted from the extension bundle.
 * All scripts deploy to ~/.immorterm/scripts/ (global, shared across projects).
 */
const BUNDLED_SCRIPTS = [
  // Core screen management
  'screen-auto',
  'screen-cleanup',
  'screen-forget',
  'screen-forget-all',
  'screen-reconcile',
  'kill-screens',
  'log-cleanup',
  // Claude session tracking (for resume after restart)
  'claude-session-capture',
  'claude-session-map',
  'claude-session-sync',
  'claude-session-tracker',
  // Claude stats
  'claude-stats',
] as const;
const BUNDLED_TEMPLATES = ['screenrc.template'] as const;
const BUNDLED_SHELL_CONFIG = ['.zshrc', 'shell-init.zsh'] as const;

/**
 * Result of resource extraction
 */
export interface ExtractionResult {
  /** Path to the .immorterm/terminals directory (per-project) */
  terminalsDir: string;
  /** Path to the logs directory */
  logsDir: string;
  /** Path to the pending directory */
  pendingDir: string;
  /** Files that were extracted (not skipped) */
  extracted: string[];
  /** Files that were skipped (already existed) */
  skipped: string[];
}

/**
 * Extracts bundled resources from the extension — fully synchronous.
 *
 * Uses sync fs calls to avoid yielding the event loop during activation.
 * Each await point in the Extension Host gives other extensions (TS server,
 * Rust Analyzer, etc.) CPU time to allocate memory, which can push RSS
 * over the OOM threshold. Sync operations complete in <1ms each and never
 * yield, preventing interleaving with memory-hungry language servers.
 *
 * Two deployment targets:
 *   Global (~/.immorterm/scripts/):  All scripts, screenrc, shell config
 *   Per-project (<workspace>/.immorterm/terminals/):  logs/, pending/, renames/
 */
export function extractResources(
  context: vscode.ExtensionContext,
  workspaceFolder: vscode.WorkspaceFolder
): ExtractionResult {
  const workspacePath = workspaceFolder.uri.fsPath;

  // Per-project data directories
  const terminalsDir = getTerminalsDirFromConfig(workspacePath);
  const logsDir = getLogsDirFromConfig(workspacePath);
  const pendingDir = getPendingDirFromConfig(workspacePath);
  const renamesDir = getRenamesDirFromConfig(workspacePath);

  // Create per-project directories
  fs.mkdirSync(terminalsDir, { recursive: true });
  fs.mkdirSync(logsDir, { recursive: true });
  fs.mkdirSync(pendingDir, { recursive: true });
  fs.mkdirSync(renamesDir, { recursive: true });

  // Create global scripts directory
  fs.mkdirSync(IMMORTERM_SCRIPTS_DIR, { recursive: true });

  logger.debug('Created directories:', { terminalsDir, scripts: IMMORTERM_SCRIPTS_DIR });

  // Check if we need to force update (extension version changed)
  const currentVersion = context.extension.packageJSON.version as string;
  const versionFilePath = path.join(IMMORTERM_SCRIPTS_DIR, VERSION_FILE);
  let forceUpdate = false;

  try {
    const storedVersion = fs.readFileSync(versionFilePath, 'utf-8');
    if (storedVersion.trim() !== currentVersion) {
      logger.info(`Extension updated from ${storedVersion.trim()} to ${currentVersion}, forcing resource update`);
      forceUpdate = true;
    }
  } catch {
    // Version file doesn't exist - first install or old installation
    forceUpdate = true;
  }

  const extracted: string[] = [];
  const skipped: string[] = [];

  // Get path to bundled resources
  const resourcesPath = path.join(context.extensionPath, 'resources');

  // Extract scripts → ~/.immorterm/scripts/
  for (const scriptName of BUNDLED_SCRIPTS) {
    const sourcePath = path.join(resourcesPath, scriptName);
    const targetPath = getGlobalScriptPath(scriptName);

    const result = extractFile(sourcePath, targetPath, true, forceUpdate);
    if (result === 'extracted') {
      extracted.push(scriptName);
    } else {
      skipped.push(scriptName);
    }
  }

  // Extract templates (screenrc.template → screenrc)
  // Themed copy → per-project (<workspace>/.immorterm/screenrc)
  // Plain copy → global fallback (~/.immorterm/scripts/screenrc)
  for (const templateName of BUNDLED_TEMPLATES) {
    const sourcePath = path.join(resourcesPath, templateName);
    const targetName = templateName.replace('.template', '');

    if (templateName === 'screenrc.template') {
      // Per-project: themed screenrc
      const projectTargetPath = getProjectScreenrcPath(workspacePath);
      const result = extractScreenrcWithTheme(sourcePath, projectTargetPath, forceUpdate);
      if (result === 'extracted') {
        extracted.push(targetName);
      } else {
        skipped.push(targetName);
      }

      // Global fallback: plain (unthemed) copy from template
      const globalTargetPath = getGlobalScriptPath(targetName);
      extractFile(sourcePath, globalTargetPath, false, forceUpdate);
    } else {
      const targetPath = getGlobalScriptPath(targetName);
      const result = extractFile(sourcePath, targetPath, false, forceUpdate);
      if (result === 'extracted') {
        extracted.push(targetName);
      } else {
        skipped.push(targetName);
      }
    }
  }

  // Extract shell config files → global
  for (const configName of BUNDLED_SHELL_CONFIG) {
    const sourcePath = path.join(resourcesPath, configName);
    const targetPath = getGlobalScriptPath(configName);

    const result = extractFile(sourcePath, targetPath, false, forceUpdate);
    if (result === 'extracted') {
      extracted.push(configName);
    } else {
      skipped.push(configName);
    }
  }

  // Extract Speak Mode character files → ~/.immorterm/characters/
  // Copies every *.md file from resources/characters/ so the UserPromptSubmit
  // hook can locate them without needing the source repo on disk.
  const charactersSrc = path.join(resourcesPath, 'characters');
  const charactersDst = path.join(IMMORTERM_GLOBAL_DIR, 'characters');
  try {
    if (fs.existsSync(charactersSrc)) {
      fs.mkdirSync(charactersDst, { recursive: true });
      for (const entry of fs.readdirSync(charactersSrc)) {
        if (!entry.endsWith('.md')) continue;
        const source = path.join(charactersSrc, entry);
        const target = path.join(charactersDst, entry);
        const result = extractFile(source, target, false, forceUpdate);
        if (result === 'extracted') extracted.push('characters/' + entry);
      }
    }
  } catch (e) {
    logger.warn('Failed to extract character files:', e);
  }

  // Extract Claude Code skills → <workspace>/.claude/skills/<name>/SKILL.md
  // Skills are plain markdown — no per-project interpolation needed — so they
  // live as real files in resources/skills/ (editable with syntax highlight)
  // rather than as TS template literals in hook-installer.ts. Any directory
  // under resources/skills/ containing a SKILL.md gets deployed.
  const skillsSrc = path.join(resourcesPath, 'skills');
  const skillsDst = path.join(workspacePath, '.claude', 'skills');
  try {
    if (fs.existsSync(skillsSrc)) {
      for (const skillName of fs.readdirSync(skillsSrc)) {
        const skillSrcDir = path.join(skillsSrc, skillName);
        if (!fs.statSync(skillSrcDir).isDirectory()) continue;
        const skillFile = path.join(skillSrcDir, 'SKILL.md');
        if (!fs.existsSync(skillFile)) continue;
        const skillDstDir = path.join(skillsDst, skillName);
        fs.mkdirSync(skillDstDir, { recursive: true });
        const target = path.join(skillDstDir, 'SKILL.md');
        const result = extractFile(skillFile, target, false, forceUpdate);
        if (result === 'extracted') extracted.push('skills/' + skillName);
      }
    }
  } catch (e) {
    logger.warn('Failed to extract skill files:', e);
  }

  // Write current version to version file (in global scripts dir)
  fs.writeFileSync(versionFilePath, currentVersion, 'utf-8');

  logger.info('Resource extraction complete:', {
    extracted: extracted.length,
    skipped: skipped.length,
    forceUpdate,
    version: currentVersion,
  });

  return {
    terminalsDir,
    logsDir,
    pendingDir,
    extracted,
    skipped,
  };
}

/**
 * Extracts a single file from bundle to target location (sync)
 */
function extractFile(
  sourcePath: string,
  targetPath: string,
  makeExecutable: boolean,
  forceOverwrite: boolean = false
): 'extracted' | 'skipped' {
  if (!forceOverwrite) {
    if (fs.existsSync(targetPath)) {
      logger.debug('Skipping existing file:', targetPath);
      return 'skipped';
    }
  }

  if (!fs.existsSync(sourcePath)) {
    logger.warn('Bundled resource not found:', sourcePath);
    return 'skipped';
  }

  try {
    fs.copyFileSync(sourcePath, targetPath);
    if (makeExecutable && process.platform !== 'win32') {
      fs.chmodSync(targetPath, 0o755);
    }
    logger.debug('Extracted file:', targetPath, makeExecutable ? '(executable)' : '', forceOverwrite ? '(force updated)' : '');
    return 'extracted';
  } catch (error) {
    logger.error('Failed to extract file:', sourcePath, '->', targetPath, error);
    return 'skipped';
  }
}

/**
 * Extracts screenrc template with the selected theme applied (sync)
 */
function extractScreenrcWithTheme(
  sourcePath: string,
  targetPath: string,
  forceOverwrite: boolean = false
): 'extracted' | 'skipped' {
  let existingContent: string | null = null;
  try {
    existingContent = fs.readFileSync(targetPath, 'utf-8');
    if (forceOverwrite) {
      logger.debug('Updating existing screenrc (force overwrite):', targetPath);
    }
  } catch {
    // File doesn't exist, proceed with extraction from template
  }

  if (!existingContent && !fs.existsSync(sourcePath)) {
    logger.warn('Bundled screenrc template not found:', sourcePath);
    return 'skipped';
  }

  try {
    // Use existing file as base (preserves user customizations), or template for new files
    const baseContent = existingContent && !forceOverwrite
      ? existingContent
      : fs.readFileSync(sourcePath, 'utf-8');

    const config = vscode.workspace.getConfiguration('immorterm');
    const themeName = config.get<string>('statusBarTheme', 'Purple Haze');
    const theme = getTheme(themeName);

    logger.debug('Applying theme to screenrc:', themeName);

    const themedHardstatus = `hardstatus alwayslastline ${generateHardstatus(theme)}`;
    let themedContent = baseContent.replace(
      /^hardstatus alwayslastline .+$/m,
      themedHardstatus
    );

    // Wire scrollbackBuffer setting into defscrollback
    const scrollback = getScrollbackBuffer();
    themedContent = themedContent.replace(
      /^defscrollback \d+$/m,
      `defscrollback ${scrollback}`
    );

    fs.writeFileSync(targetPath, themedContent, 'utf-8');
    logger.debug('Extracted themed screenrc:', targetPath, 'with theme:', themeName, 'scrollback:', scrollback);
    return 'extracted';
  } catch (error) {
    logger.error('Failed to extract themed screenrc:', sourcePath, '->', targetPath, error);
    return 'skipped';
  }
}

/**
 * Gets the path to the terminals directory for a workspace
 * @returns Path to .immorterm/terminals/
 */
export function getTerminalsDir(workspaceFolder: vscode.WorkspaceFolder): string {
  return getTerminalsDirFromConfig(workspaceFolder.uri.fsPath);
}

/**
 * Gets the path to the logs directory for a workspace
 * @returns Path to .immorterm/terminals/logs/
 */
export function getLogsDir(workspaceFolder: vscode.WorkspaceFolder): string {
  return getLogsDirFromConfig(workspaceFolder.uri.fsPath);
}

/**
 * Gets the path to the pending directory for a workspace
 * @returns Path to .immorterm/terminals/pending/
 */
export function getPendingDir(workspaceFolder: vscode.WorkspaceFolder): string {
  return getPendingDirFromConfig(workspaceFolder.uri.fsPath);
}

/**
 * Gets the path to the global scripts directory.
 * Scripts are shared across all projects.
 * @returns Path to ~/.immorterm/scripts/
 */
export function getScriptsDir(): string {
  return IMMORTERM_SCRIPTS_DIR;
}

/**
 * Gets the path to a specific script (now global)
 */
export function getScriptPath(
  _workspaceFolder: vscode.WorkspaceFolder,
  scriptName: string
): string {
  return getGlobalScriptPath(scriptName);
}

/**
 * Checks if resources have been extracted
 * @returns true if screen-auto exists in global scripts dir
 */
export function areResourcesExtracted(
  _workspaceFolder: vscode.WorkspaceFolder
): boolean {
  const screenAutoPath = getGlobalScriptPath('screen-auto');
  return fs.existsSync(screenAutoPath);
}

export default {
  extractResources,
  getTerminalsDir,
  getLogsDir,
  getPendingDir,
  getScriptsDir,
  getScriptPath,
  areResourcesExtracted,
};
