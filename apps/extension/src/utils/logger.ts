import * as vscode from 'vscode';
import { isDebugLogEnabled } from './settings';

/**
 * Logger utility for ImmorTerm extension
 * Creates an OutputChannel named "ImmorTerm" and supports multiple log levels.
 * Respects the immorterm.enableDebugLog setting via the settings utility.
 */

type LogLevel = 'debug' | 'info' | 'warn' | 'error';

const LOG_LEVEL_PRIORITY: Record<LogLevel, number> = {
  debug: 0,
  info: 1,
  warn: 2,
  error: 3,
};

let outputChannel: vscode.OutputChannel | undefined;

/**
 * Gets or creates the ImmorTerm output channel
 */
function getOutputChannel(): vscode.OutputChannel {
  if (!outputChannel) {
    outputChannel = vscode.window.createOutputChannel('ImmorTerm');
  }
  return outputChannel;
}

/**
 * Checks if debug logging is enabled via settings utility
 */
function isDebugEnabled(): boolean {
  return isDebugLogEnabled();
}

/**
 * Formats a log message with timestamp and level
 */
function formatMessage(level: LogLevel, message: string): string {
  const timestamp = new Date().toISOString();
  const levelStr = level.toUpperCase().padEnd(5);
  return `[${timestamp}] [${levelStr}] ${message}`;
}

/**
 * Logs a message at the specified level
 */
function log(level: LogLevel, message: string, ...args: unknown[]): void {
  // Skip debug messages if debug logging is disabled
  if (level === 'debug' && !isDebugEnabled()) {
    return;
  }

  const channel = getOutputChannel();

  // Format the message with any additional arguments
  let fullMessage = message;
  if (args.length > 0) {
    const formattedArgs = args.map(arg => {
      if (typeof arg === 'object') {
        try {
          return JSON.stringify(arg, null, 2);
        } catch {
          return String(arg);
        }
      }
      return String(arg);
    }).join(' ');
    fullMessage = `${message} ${formattedArgs}`;
  }

  const formattedMessage = formatMessage(level, fullMessage);
  channel.appendLine(formattedMessage);

  // Also log to console for development
  if (level === 'error') {
    console.error(formattedMessage);
  } else if (level === 'warn') {
    console.warn(formattedMessage);
  }
}

/**
 * Logger interface for ImmorTerm
 */
export const logger = {
  /**
   * Log a debug message (only when enableDebugLog is true)
   */
  debug(message: string, ...args: unknown[]): void {
    log('debug', message, ...args);
  },

  /**
   * Log an info message
   */
  info(message: string, ...args: unknown[]): void {
    log('info', message, ...args);
  },

  /**
   * Log a warning message
   */
  warn(message: string, ...args: unknown[]): void {
    log('warn', message, ...args);
  },

  /**
   * Log an error message
   */
  error(message: string, ...args: unknown[]): void {
    log('error', message, ...args);
  },

  /**
   * Show the output channel to the user
   */
  show(): void {
    getOutputChannel().show();
  },

  /**
   * Dispose of the output channel
   */
  dispose(): void {
    if (outputChannel) {
      outputChannel.dispose();
      outputChannel = undefined;
    }
  },

  /**
   * Get the output channel for direct access (e.g., for subscriptions)
   */
  getChannel(): vscode.OutputChannel {
    return getOutputChannel();
  },
};

export default logger;
