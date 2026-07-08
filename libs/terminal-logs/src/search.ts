/**
 * Search — Full-text search across grid.jsonl files
 *
 * Streams snapshots from NDJSON files, strips ANSI attributes to plain text,
 * and matches against a case-insensitive substring query.
 */

import type { AttributeRun, GridSnapshot } from "./schema.js";
import { GridSnapshotSchema } from "./schema.js";
import { readNdjson } from "./reader.js";
import { stripRuns } from "./ansi.js";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export interface SearchMatch {
	/** Index of the snapshot within the file */
	snapshotIndex: number;
	/** Snapshot metadata */
	snapshot: {
		ts: number;
		trigger: string;
		cwd: string;
		cols: number;
		rows: number;
	};
	/** Row number within the grid where match was found */
	row: number;
	/** Plain text of the matching row */
	plainText: string;
	/** Original AttributeRun[] for colored rendering */
	runs: AttributeRun[];
	/** Character offset of match within the plain text */
	matchOffset: number;
	/** Length of the matched text */
	matchLength: number;
}

export interface SearchResult {
	/** Absolute path to the .grid.jsonl file */
	filePath: string;
	/** All matches found in this file */
	matches: SearchMatch[];
}

// ---------------------------------------------------------------------------
// searchGridFile — Search a single .grid.jsonl file
// ---------------------------------------------------------------------------

export async function searchGridFile(
	filePath: string,
	query: string,
	maxMatches = 100,
): Promise<SearchResult> {
	const lowerQuery = query.toLowerCase();
	const matches: SearchMatch[] = [];
	let snapshotIndex = 0;

	for await (const snapshot of readNdjson<GridSnapshot>(filePath, GridSnapshotSchema)) {
		for (const rowRuns of snapshot.grid) {
			if (matches.length >= maxMatches) break;

			const plainText = stripRuns(rowRuns.runs);
			const lowerText = plainText.toLowerCase();
			let offset = 0;

			// Find all occurrences in this row
			while (offset < lowerText.length) {
				const idx = lowerText.indexOf(lowerQuery, offset);
				if (idx === -1) break;

				matches.push({
					snapshotIndex,
					snapshot: {
						ts: snapshot.ts,
						trigger: snapshot.trigger,
						cwd: snapshot.cwd,
						cols: snapshot.cols,
						rows: snapshot.rows,
					},
					row: rowRuns.row,
					plainText,
					runs: rowRuns.runs,
					matchOffset: idx,
					matchLength: query.length,
				});

				offset = idx + query.length;
				if (matches.length >= maxMatches) break;
			}
		}

		snapshotIndex++;
		if (matches.length >= maxMatches) break;
	}

	return { filePath, matches };
}

// ---------------------------------------------------------------------------
// searchMultipleFiles — Search across multiple .grid.jsonl files concurrently
// ---------------------------------------------------------------------------

export async function searchMultipleFiles(
	filePaths: string[],
	query: string,
	maxMatchesPerFile = 50,
): Promise<SearchResult[]> {
	const results = await Promise.all(
		filePaths.map((fp) => searchGridFile(fp, query, maxMatchesPerFile)),
	);
	return results.filter((r) => r.matches.length > 0);
}
