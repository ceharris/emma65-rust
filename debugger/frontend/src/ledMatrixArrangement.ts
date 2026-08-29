/**
 * Physical LED matrix panel arrangements — mirrors `preferences::LedMatrixArrangement` field for
 * field. Real matrix boards are daisy-chained, so any factorization of the configured matrix count
 * into `columns * rows` is a wireable layout (2 matrices: 2x1 or 1x2; 4: 4x1, 2x2, or 1x4; 8: 8x1,
 * 4x2, 2x4, or 1x8; and so on for any other count) — `arrangementsForCount` computes that list
 * generically rather than hardcoding it per count.
 */

export interface LedMatrixArrangement {
  columns: number;
  rows: number;
}

/** The single-row layout `LedMatrixPanel.tsx` used before the arrangement menu existed — the
 * fallback whenever no valid arrangement has been chosen yet. */
export function defaultArrangement(matrices: number): LedMatrixArrangement {
  return { columns: matrices, rows: 1 };
}

/**
 * Every way `matrices` physical matrices can be wired as a rectangular grid, widest first —
 * every divisor pair of `matrices`, `columns` descending. For `matrices = 8` this yields exactly
 * `8x1, 4x2, 2x4, 1x8`, matching how real boards are described.
 */
export function arrangementsForCount(matrices: number): LedMatrixArrangement[] {
  const arrangements: LedMatrixArrangement[] = [];
  for (let columns = matrices; columns >= 1; columns--) {
    if (matrices % columns === 0) arrangements.push({ columns, rows: matrices / columns });
  }
  return arrangements;
}

/** Whether `arrangement` is a valid layout for `matrices` physical matrices — false for `null`
 * (nothing chosen yet) and for a stale arrangement left over from a profile with a different
 * matrix count. */
export function isValidArrangement(arrangement: LedMatrixArrangement | null, matrices: number): boolean {
  return arrangement !== null && arrangement.columns >= 1 && arrangement.rows >= 1 && arrangement.columns * arrangement.rows === matrices;
}
