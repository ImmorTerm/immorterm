const MAIN_STYLESHEET = '<link rel="stylesheet" href="__CSS_URI__">';
const CODICON_STYLESHEET = '<link rel="stylesheet" href="__CODICON_CSS_URI__">';

function safeStyleText(css: string): string {
  return css.replace(/<\/style/gi, '<\\/style');
}

/**
 * Produce a self-contained style payload for the VS Code WebView.
 *
 * VS Code's resource service can reject or lose external stylesheet requests
 * independently of the HTML/module load. Inlining keeps layout atomic with the
 * document; the Codicon font is embedded too so controls cannot fall back to
 * raw text-sized glyphs.
 */
export function inlineWebviewStyles(
  html: string,
  mainCss: string,
  codiconCss: string,
  codiconFont: Buffer,
): string {
  const fontDataUri = `data:font/ttf;base64,${codiconFont.toString('base64')}`;
  const embeddedCodicons = codiconCss.replace(
    /url\((?:"|')?\.\/codicon\.ttf[^)]*\)/,
    `url("${fontDataUri}")`,
  );

  if (!html.includes(MAIN_STYLESHEET) || !html.includes(CODICON_STYLESHEET)) {
    throw new Error('WebView stylesheet placeholders are missing');
  }

  return html
    .replace(MAIN_STYLESHEET, `<style data-immorterm-style="main">${safeStyleText(mainCss)}</style>`)
    .replace(
      CODICON_STYLESHEET,
      `<style data-immorterm-style="codicons">${safeStyleText(embeddedCodicons)}</style>`,
    );
}
