/* tslint:disable */
/* eslint-disable */

/**
 * Chroma subsampling format
 */
export enum ChromaSampling {
    /**
     * Both vertically and horizontally subsampled.
     */
    Cs420 = 0,
    /**
     * Horizontally subsampled.
     */
    Cs422 = 1,
    /**
     * Not subsampled.
     */
    Cs444 = 2,
    /**
     * Monochrome.
     */
    Cs400 = 3,
}

export class WasmTerminal {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Stage a comment anchored at an explicit content-coord range
     * `[sr, sc, er, ec]`. Bypasses the live-selection / pseudo-cursor
     * guards used by `add_comment_for_selection` — required by the
     * Cmd+E wizard which iterates over multiple pre-detected ranges
     * while pseudo-cursors are still held for visualization. Returns
     * the new comment id, or 0 on failure.
     */
    add_comment_for_range(sr: number, sc: number, er: number, ec: number, comment_text: string, created_at_ms: number): number;
    /**
     * Stage a new comment anchored at the current selection. Returns the
     * new comment id (>0), or 0 if there is no active selection.
     */
    add_comment_for_selection(comment_text: string, created_at_ms: number): number;
    cell_grapheme_at(grid_row: number, col: number): string;
    cell_size_device(): Float32Array;
    /**
     * Drop every staged comment.
     */
    clear_comments(): void;
    clear_scrollback(): void;
    click_to_cursor_correction_seq(target_col: number, target_grid_row: number): Uint8Array;
    click_to_cursor_plan(css_x: number, css_y: number): string;
    click_to_cursor_sequence(css_x: number, css_y: number): Uint8Array;
    /**
     * Number of staged comments (may include orphaned ones).
     */
    comments_count(): number;
    create_background(json: string): number;
    /**
     * Current working directory (see inner `cwd()` for details).
     */
    cwd(): string;
    debug_bidi_row(): string;
    /**
     * Diagnostic: returns a JSON string describing what the bullet
     * detector saw. Helps debug why detect_claude_bullets returned empty.
     * Includes: scanned row count, found_sentinel flag, candidate count,
     * and the first 5 candidate previews. Call from JS console.
     */
    debug_claude_bullets(): string;
    debug_click_info(css_x: number, css_y: number): string;
    debug_click_trace(css_x: number, css_y: number): string;
    debug_cursor(): string;
    debug_selection_wrapped(): string;
    debug_theme(): string;
    delete_selection_sequence(): Uint8Array;
    destroy_background(id: number): void;
    /**
     * Detect bold/emphasised text runs in the visible viewport.
     * Used by the Cmd+D no-selection path so the user can turn
     * each emphasised phrase into a task. Returns flat
     * `[sr, sc, er, ec, ...]` ranges. Requires ≥2 runs to fire.
     */
    detect_bold_runs_viewport(): Uint32Array;
    /**
     * Detect bullet titles in the current Claude turn (bottom-up scan
     * bounded by a `❯` user-prompt sentinel). Returns a flat array of
     * `[start_row, start_col, end_row, end_col]` quads in absolute
     * content coords, or an empty array if no confident match is found.
     */
    detect_claude_bullets(): Uint32Array;
    /**
     * Fallback bullet detection over just the visible viewport. Use
     * when `detect_claude_bullets` returns empty (e.g. the user has
     * scrolled to an older turn). Same `≥2 sibling` rule applies.
     */
    detect_claude_bullets_viewport(): Uint32Array;
    dimensions(): Uint32Array;
    drop_background(id: number): void;
    encode_mouse_event(button: number, pressed: boolean, css_x: number, css_y: number): Uint8Array;
    get_ai_primitives_json(): string;
    get_pseudo_cursor_text(): string;
    get_selected_html(): string;
    get_selected_text(): string;
    handle_key(key: string, ctrl: boolean, shift: boolean, alt: boolean): Uint8Array;
    has_pseudo_cursors(): boolean;
    has_selection(): boolean;
    init_gpu(canvas_id: string, dpr: number): Promise<void>;
    link_at(css_x: number, css_y: number): string;
    /**
     * All staged comments as a JSON array string.
     * Schema matches `comments::Comment`.
     */
    list_comments_json(): string;
    load_snapshot(json: string, immorterm_id: string): void;
    load_snapshot_background(id: number, json: string): void;
    mouse_tracking_enabled(): boolean;
    constructor(cols: number, rows: number);
    paragraph_direction(): string;
    paste_undo_probe(): string;
    prepend_scrollback(json: string): void;
    process(data: Uint8Array): void;
    process_background(id: number, data: Uint8Array): boolean;
    process_str(text: string): void;
    pseudo_cursor_add(css_x: number, css_y: number): void;
    pseudo_cursor_add_at(col: number, content_row: number): void;
    pseudo_cursor_add_at_visual_cursor(): void;
    pseudo_cursor_add_vertical(direction: string): void;
    pseudo_cursor_clear(): void;
    pseudo_cursor_count(): number;
    pseudo_cursor_extend_all(direction: string): void;
    /**
     * Replace pseudo-cursors with N range pseudo-selections, one per
     * `[start_row, start_col, end_row, end_col]` quad in `flat`. Used by
     * the Cmd+E auto-comment flow to visualize detected bullet titles.
     */
    pseudo_select_ranges(flat: Uint32Array): void;
    /**
     * Read the visible text of a content-coord range. Returns an
     * empty string if the range is invalid.
     */
    read_range_text(sr: number, sc: number, er: number, ec: number): string;
    reinit_renderer(new_dpr: number): Uint32Array;
    /**
     * Remove a staged comment by id. Returns true if found.
     */
    remove_comment(id: number): boolean;
    render(): boolean;
    resize(width: number, height: number): Uint32Array;
    resize_backgrounds(): void;
    restore(id: number): void;
    save_active(): number;
    scroll(delta: number): boolean;
    scroll_offset(): number;
    scroll_to_bottom(): void;
    scrollback_len(): number;
    select_all_input(): boolean;
    select_line_at(css_x: number, css_y: number): void;
    select_word_at(css_x: number, css_y: number): void;
    selection_clear(): void;
    /**
     * Current selection as `[start_row, start_col, end_row, end_col]` in
     * absolute content coordinates. Empty if no selection.
     */
    selection_content_range(): Uint32Array;
    selection_extend(direction: string): void;
    selection_start(css_x: number, css_y: number): void;
    selection_start_block(css_x: number, css_y: number): void;
    selection_update(css_x: number, css_y: number): void;
    set_ai_ctx_pct(pct: number): void;
    set_ai_stats(stats: string): void;
    set_animations_enabled(enabled: boolean): void;
    set_ansi_colors(colors: Float32Array): void;
    set_background_ai_stats(id: number, stats: string): void;
    set_background_session_title(id: number, title: string): void;
    set_border_enabled(enabled: boolean): void;
    set_border_opacity(opacity: number): void;
    set_celebrations_enabled(enabled: boolean): void;
    set_char_height_css(value: number): void;
    set_content_padding(top: number, right: number, bottom: number, left: number): void;
    set_custom_font(data: Uint8Array): void;
    set_custom_font_name(name: string): void;
    set_danger_effects(enabled: boolean): void;
    set_expression(json: string): void;
    set_expression_effects(enabled: boolean): void;
    set_font_size(size: number): void;
    set_font_weight(weight: number): void;
    set_immorterm_id(id: string): void;
    set_line_height(value: number): void;
    set_paragraph_direction(direction: string): void;
    set_project_name(name: string): void;
    set_scroll_indicator_proximity(proximity: number): void;
    set_scroll_offset(offset: number): void;
    set_selection_color(r: number, g: number, b: number, a: number): void;
    set_session_title(title: string): void;
    set_status_bar_enabled(enabled: boolean): void;
    set_status_bar_hover(target: string): void;
    set_status_bar_mode(mode: string): void;
    set_status_bar_reveal(reveal: number): void;
    set_terminal_colors(bg_r: number, bg_g: number, bg_b: number, fg_r: number, fg_g: number, fg_b: number, cursor_r: number, cursor_g: number, cursor_b: number): void;
    set_text_alignment(alignment: string): void;
    set_text_animations(enabled: boolean): void;
    set_theme(name: string): void;
    status_bar_hit_test(col: number): string;
    take_bell(): boolean;
    text_alignment(): string;
    title(): string;
    update_ai_primitives(json: string, daemon_sb_len: number): void;
    /**
     * Update a staged comment's body text.
     */
    update_comment_text(id: number, new_text: string): boolean;
    /**
     * Visible comment anchors for the current frame, packed as
     * `[display_row, col_start, col_end, id, ...]`. JS positions a
     * sidebar pill per entry using the same math as emoji overlays.
     */
    visible_comment_anchors(): Uint32Array;
    visible_emoji_cells(): Uint32Array;
    visible_rows(): number;
    visual_cursor_display(): Int32Array;
}

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_wasmterminal_free: (a: number, b: number) => void;
    readonly wasmterminal_add_comment_for_range: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => number;
    readonly wasmterminal_add_comment_for_selection: (a: number, b: number, c: number, d: number) => number;
    readonly wasmterminal_cell_grapheme_at: (a: number, b: number, c: number) => [number, number];
    readonly wasmterminal_cell_size_device: (a: number) => [number, number];
    readonly wasmterminal_clear_comments: (a: number) => void;
    readonly wasmterminal_clear_scrollback: (a: number) => void;
    readonly wasmterminal_click_to_cursor_correction_seq: (a: number, b: number, c: number) => [number, number];
    readonly wasmterminal_click_to_cursor_plan: (a: number, b: number, c: number) => [number, number];
    readonly wasmterminal_click_to_cursor_sequence: (a: number, b: number, c: number) => [number, number];
    readonly wasmterminal_comments_count: (a: number) => number;
    readonly wasmterminal_create_background: (a: number, b: number, c: number) => [number, number, number];
    readonly wasmterminal_cwd: (a: number) => [number, number];
    readonly wasmterminal_debug_bidi_row: (a: number) => [number, number];
    readonly wasmterminal_debug_claude_bullets: (a: number) => [number, number];
    readonly wasmterminal_debug_click_info: (a: number, b: number, c: number) => [number, number];
    readonly wasmterminal_debug_click_trace: (a: number, b: number, c: number) => [number, number];
    readonly wasmterminal_debug_cursor: (a: number) => [number, number];
    readonly wasmterminal_debug_selection_wrapped: (a: number) => [number, number];
    readonly wasmterminal_debug_theme: (a: number) => [number, number];
    readonly wasmterminal_delete_selection_sequence: (a: number) => [number, number];
    readonly wasmterminal_destroy_background: (a: number, b: number) => void;
    readonly wasmterminal_detect_bold_runs_viewport: (a: number) => [number, number];
    readonly wasmterminal_detect_claude_bullets: (a: number) => [number, number];
    readonly wasmterminal_detect_claude_bullets_viewport: (a: number) => [number, number];
    readonly wasmterminal_dimensions: (a: number) => [number, number];
    readonly wasmterminal_drop_background: (a: number, b: number) => [number, number];
    readonly wasmterminal_encode_mouse_event: (a: number, b: number, c: number, d: number, e: number) => [number, number];
    readonly wasmterminal_get_ai_primitives_json: (a: number) => [number, number];
    readonly wasmterminal_get_pseudo_cursor_text: (a: number) => [number, number];
    readonly wasmterminal_get_selected_html: (a: number) => [number, number];
    readonly wasmterminal_get_selected_text: (a: number) => [number, number];
    readonly wasmterminal_handle_key: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number];
    readonly wasmterminal_has_pseudo_cursors: (a: number) => number;
    readonly wasmterminal_has_selection: (a: number) => number;
    readonly wasmterminal_init_gpu: (a: number, b: number, c: number, d: number) => any;
    readonly wasmterminal_link_at: (a: number, b: number, c: number) => [number, number];
    readonly wasmterminal_list_comments_json: (a: number) => [number, number];
    readonly wasmterminal_load_snapshot: (a: number, b: number, c: number, d: number, e: number) => [number, number];
    readonly wasmterminal_load_snapshot_background: (a: number, b: number, c: number, d: number) => [number, number];
    readonly wasmterminal_mouse_tracking_enabled: (a: number) => number;
    readonly wasmterminal_new: (a: number, b: number) => number;
    readonly wasmterminal_paragraph_direction: (a: number) => [number, number];
    readonly wasmterminal_paste_undo_probe: (a: number) => [number, number];
    readonly wasmterminal_prepend_scrollback: (a: number, b: number, c: number) => [number, number];
    readonly wasmterminal_process: (a: number, b: number, c: number) => void;
    readonly wasmterminal_process_background: (a: number, b: number, c: number, d: number) => [number, number, number];
    readonly wasmterminal_process_str: (a: number, b: number, c: number) => void;
    readonly wasmterminal_pseudo_cursor_add: (a: number, b: number, c: number) => void;
    readonly wasmterminal_pseudo_cursor_add_at: (a: number, b: number, c: number) => void;
    readonly wasmterminal_pseudo_cursor_add_at_visual_cursor: (a: number) => void;
    readonly wasmterminal_pseudo_cursor_add_vertical: (a: number, b: number, c: number) => void;
    readonly wasmterminal_pseudo_cursor_clear: (a: number) => void;
    readonly wasmterminal_pseudo_cursor_count: (a: number) => number;
    readonly wasmterminal_pseudo_cursor_extend_all: (a: number, b: number, c: number) => void;
    readonly wasmterminal_pseudo_select_ranges: (a: number, b: number, c: number) => void;
    readonly wasmterminal_read_range_text: (a: number, b: number, c: number, d: number, e: number) => [number, number];
    readonly wasmterminal_reinit_renderer: (a: number, b: number) => [number, number, number, number];
    readonly wasmterminal_remove_comment: (a: number, b: number) => number;
    readonly wasmterminal_render: (a: number) => number;
    readonly wasmterminal_resize: (a: number, b: number, c: number) => [number, number];
    readonly wasmterminal_resize_backgrounds: (a: number) => void;
    readonly wasmterminal_restore: (a: number, b: number) => [number, number];
    readonly wasmterminal_save_active: (a: number) => number;
    readonly wasmterminal_scroll: (a: number, b: number) => number;
    readonly wasmterminal_scroll_offset: (a: number) => number;
    readonly wasmterminal_scroll_to_bottom: (a: number) => void;
    readonly wasmterminal_scrollback_len: (a: number) => number;
    readonly wasmterminal_select_all_input: (a: number) => number;
    readonly wasmterminal_select_line_at: (a: number, b: number, c: number) => void;
    readonly wasmterminal_select_word_at: (a: number, b: number, c: number) => void;
    readonly wasmterminal_selection_clear: (a: number) => void;
    readonly wasmterminal_selection_content_range: (a: number) => [number, number];
    readonly wasmterminal_selection_extend: (a: number, b: number, c: number) => void;
    readonly wasmterminal_selection_start: (a: number, b: number, c: number) => void;
    readonly wasmterminal_selection_start_block: (a: number, b: number, c: number) => void;
    readonly wasmterminal_selection_update: (a: number, b: number, c: number) => void;
    readonly wasmterminal_set_ai_ctx_pct: (a: number, b: number) => void;
    readonly wasmterminal_set_ai_stats: (a: number, b: number, c: number) => void;
    readonly wasmterminal_set_animations_enabled: (a: number, b: number) => void;
    readonly wasmterminal_set_ansi_colors: (a: number, b: number, c: number) => void;
    readonly wasmterminal_set_background_ai_stats: (a: number, b: number, c: number, d: number) => void;
    readonly wasmterminal_set_background_session_title: (a: number, b: number, c: number, d: number) => void;
    readonly wasmterminal_set_border_enabled: (a: number, b: number) => void;
    readonly wasmterminal_set_border_opacity: (a: number, b: number) => void;
    readonly wasmterminal_set_celebrations_enabled: (a: number, b: number) => void;
    readonly wasmterminal_set_char_height_css: (a: number, b: number) => void;
    readonly wasmterminal_set_content_padding: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly wasmterminal_set_custom_font: (a: number, b: number, c: number) => void;
    readonly wasmterminal_set_custom_font_name: (a: number, b: number, c: number) => void;
    readonly wasmterminal_set_danger_effects: (a: number, b: number) => void;
    readonly wasmterminal_set_expression: (a: number, b: number, c: number) => [number, number];
    readonly wasmterminal_set_expression_effects: (a: number, b: number) => void;
    readonly wasmterminal_set_font_size: (a: number, b: number) => void;
    readonly wasmterminal_set_font_weight: (a: number, b: number) => void;
    readonly wasmterminal_set_immorterm_id: (a: number, b: number, c: number) => void;
    readonly wasmterminal_set_line_height: (a: number, b: number) => void;
    readonly wasmterminal_set_paragraph_direction: (a: number, b: number, c: number) => void;
    readonly wasmterminal_set_project_name: (a: number, b: number, c: number) => void;
    readonly wasmterminal_set_scroll_indicator_proximity: (a: number, b: number) => void;
    readonly wasmterminal_set_scroll_offset: (a: number, b: number) => void;
    readonly wasmterminal_set_selection_color: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly wasmterminal_set_session_title: (a: number, b: number, c: number) => void;
    readonly wasmterminal_set_status_bar_enabled: (a: number, b: number) => void;
    readonly wasmterminal_set_status_bar_hover: (a: number, b: number, c: number) => void;
    readonly wasmterminal_set_status_bar_mode: (a: number, b: number, c: number) => void;
    readonly wasmterminal_set_status_bar_reveal: (a: number, b: number) => void;
    readonly wasmterminal_set_terminal_colors: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number) => void;
    readonly wasmterminal_set_text_alignment: (a: number, b: number, c: number) => void;
    readonly wasmterminal_set_text_animations: (a: number, b: number) => void;
    readonly wasmterminal_set_theme: (a: number, b: number, c: number) => void;
    readonly wasmterminal_status_bar_hit_test: (a: number, b: number) => [number, number];
    readonly wasmterminal_take_bell: (a: number) => number;
    readonly wasmterminal_text_alignment: (a: number) => [number, number];
    readonly wasmterminal_title: (a: number) => [number, number];
    readonly wasmterminal_update_ai_primitives: (a: number, b: number, c: number, d: number) => void;
    readonly wasmterminal_update_comment_text: (a: number, b: number, c: number, d: number) => number;
    readonly wasmterminal_visible_comment_anchors: (a: number) => [number, number];
    readonly wasmterminal_visible_emoji_cells: (a: number) => [number, number];
    readonly wasmterminal_visible_rows: (a: number) => number;
    readonly wasmterminal_visual_cursor_display: (a: number) => [number, number];
    readonly wasm_bindgen__convert__closures_____invoke__h2bf5b722dd9eda8c: (a: number, b: number, c: any) => [number, number];
    readonly wasm_bindgen__convert__closures_____invoke__h94414e4fd4c5bce5: (a: number, b: number, c: any, d: any) => void;
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __wbindgen_exn_store: (a: number) => void;
    readonly __externref_table_alloc: () => number;
    readonly __wbindgen_free: (a: number, b: number, c: number) => void;
    readonly __wbindgen_destroy_closure: (a: number, b: number) => void;
    readonly __externref_table_dealloc: (a: number) => void;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
