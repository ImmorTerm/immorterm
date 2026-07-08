
export async function register_font_face(name, data, weight, style) {
    try {
        const buf = new Uint8Array(data).buffer;
        const font = new FontFace(name, buf, { weight: weight || 'normal', style: style || 'normal' });
        await font.load();
        document.fonts.add(font);
        console.log(`[FontFace] Registered '${name}' (weight=${weight}, style=${style}, ${data.length} bytes)`);
    } catch (e) {
        console.error(`[FontFace] FAILED '${name}': ${e.message}`);
    }
}
